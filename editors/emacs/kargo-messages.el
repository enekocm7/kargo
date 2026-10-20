;;; kargo-messages.el --- Read the JSON messages of Kargo  -*- lexical-binding: t; -*-

;; SPDX-License-Identifier: MIT OR Apache-2.0

;; Author: The Kargo authors
;; URL: https://github.com/enekocm7/kargo
;; Package-Requires: ((emacs "29.1"))

;;; Commentary:

;; `kargo build', `check', `run' and `test' print one JSON object per line with
;; `--message-format json': compiler diagnostics, test results and summaries.  This file
;; decodes those lines and turns them into the `file:line:column: error: text' lines that
;; `compilation-mode' understands.  It has no dependencies on Kargo or on a project, so the
;; tests run on fixtures.

;;; Code:

(require 'cl-lib)
(require 'json)
(require 'subr-x)

(declare-function json-available-p "json")

(defun kargo-messages-json (text)
  "Parse TEXT as JSON.
Objects become alists with symbol keys, arrays lists, and `null' and `false'
nil.  Signal an error when TEXT is not JSON."
  (if (and (fboundp 'json-parse-string)
           (or (not (fboundp 'json-available-p)) (json-available-p)))
      (json-parse-string text :object-type 'alist :array-type 'list :null-object nil :false-object nil)
    (let ((json-object-type 'alist) (json-array-type 'list) (json-false nil) (json-null nil))
      (json-read-from-string text))))

(defun kargo-messages-parse (line)
  "Decode LINE as a Kargo message.
Return an alist with a `reason' key, or nil when LINE is not a message, for
example a status line or the output of the program."
  (when (and (> (length line) 1) (eq (aref line 0) ?{))
    (condition-case nil
        (let ((message (kargo-messages-json line)))
          (and (consp message) (alist-get 'reason message) message))
      (error nil))))

(defun kargo-messages-decode (text)
  "Decode every message in TEXT, a string of lines.  Other lines are skipped."
  (delq nil (mapcar #'kargo-messages-parse (split-string text "\n" t))))

(defun kargo-messages-severity (message)
  "The severity of the compiler MESSAGE as `error', `warning' or `info'."
  (pcase (alist-get 'severity message)
    ("warning" 'warning)
    ((or "info" "note") 'info)
    (_ 'error)))

(defun kargo-messages-format-compiler (message)
  "Format the `compiler-message' MESSAGE like a compiler does.
The first line is `file:line:column: severity: text', which `compilation-mode'
turns into a link.  The source line and caret Kargo rendered follow."
  (let* ((file (alist-get 'file message))
         (line (alist-get 'line message))
         (column (alist-get 'column message))
         (severity (symbol-name (kargo-messages-severity message)))
         (text (or (alist-get 'message message) ""))
         (rendered (alist-get 'rendered message))
         (context (and rendered (not (string-match-p "\n" text))
                       (cdr (split-string rendered "\n")))))
    (mapconcat
     #'identity
     (cons (if (and file line)
               (format "%s:%d:%s %s: %s" (expand-file-name file) line
                       (if column (format "%d:" column) "") severity text)
             (format "%s: %s" severity text))
           context)
     "\n")))

(defun kargo-messages-test-result (message)
  "The alist of a `test-finished' MESSAGE, or nil for another message.
The keys are `suite', `name', `display', `outcome', `message', `details',
`duration' and `target'."
  (when (equal (alist-get 'reason message) "test-finished")
    `((suite . ,(alist-get 'suite message))
      (name . ,(alist-get 'name message))
      (display . ,(or (alist-get 'display-name message) (alist-get 'name message)))
      (outcome . ,(alist-get 'outcome message))
      (message . ,(alist-get 'message message))
      (details . ,(alist-get 'details message))
      (duration . ,(alist-get 'duration-ms message))
      (target . ,(alist-get 'target message)))))

(defun kargo-messages-test-title (result)
  "The qualified name of the test RESULT, `Suite.name', as `kargo test' filters by."
  (let ((suite (alist-get 'suite result)))
    (if suite
        (format "%s.%s" (replace-regexp-in-string "\\$" "." suite) (alist-get 'name result))
      (alist-get 'name result))))

(defun kargo-messages-format-test (result &optional frames)
  "Format the test RESULT as a line `test Suite.name ... ok'.
A failure is followed by its message and its stack trace.  FRAMES, when
non-nil, is a list of already formatted stack trace lines to use instead of
the raw trace."
  (let* ((outcome (alist-get 'outcome result))
         (duration (alist-get 'duration result))
         (head (format "test %s ... %s%s"
                       (kargo-messages-test-title result)
                       (pcase outcome ("passed" "ok") ("failed" "FAILED") (_ "ignored"))
                       (if duration (format " (%d ms)" duration) "")))
         (text (alist-get 'message result))
         (details (alist-get 'details result)))
    (mapconcat
     #'identity
     (append (list head)
             (when (and text (not (equal outcome "passed")))
               (mapcar (lambda (line) (concat "    " line)) (split-string text "\n")))
             (when (equal outcome "failed")
               (or frames
                   (when details
                     (mapcar (lambda (line) (concat "    " (string-trim line)))
                             (split-string details "\n" t))))))
     "\n")))

(defun kargo-messages-format-summary (message)
  "Format a `test-summary' MESSAGE as a line."
  (format "test result: %d passed; %d failed; %d ignored (%s)"
          (or (alist-get 'passed message) 0)
          (or (alist-get 'failed message) 0)
          (or (alist-get 'ignored message) 0)
          (or (alist-get 'target message) "jvm")))

(defun kargo-messages-counts (results)
  "Count the outcomes of the test RESULTS as a list (PASSED FAILED IGNORED)."
  (let ((passed 0) (failed 0) (ignored 0))
    (dolist (result results)
      (pcase (alist-get 'outcome result)
        ("passed" (cl-incf passed))
        ("failed" (cl-incf failed))
        (_ (cl-incf ignored))))
    (list passed failed ignored)))

(defun kargo-messages-diagnostics (messages)
  "The compiler diagnostics of MESSAGES that have a file and a line.
Each one is a list (FILE LINE COLUMN SEVERITY TEXT); FILE is absolute, LINE and
COLUMN are 1-based (COLUMN is 1 when the compiler gave none)."
  (let (diagnostics)
    (dolist (message messages)
      (when (equal (alist-get 'reason message) "compiler-message")
        (let ((file (alist-get 'file message))
              (line (alist-get 'line message)))
          (when (and file line)
            (push (list (expand-file-name file) line (or (alist-get 'column message) 1)
                        (kargo-messages-severity message) (alist-get 'message message))
                  diagnostics)))))
    (nreverse diagnostics)))

(provide 'kargo-messages)
;;; kargo-messages.el ends here
