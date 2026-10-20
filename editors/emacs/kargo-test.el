;;; kargo-test.el --- Run Kargo tests from Emacs  -*- lexical-binding: t; -*-

;; SPDX-License-Identifier: MIT OR Apache-2.0

;; Author: The Kargo authors
;; URL: https://github.com/enekocm7/kargo
;; Package-Requires: ((emacs "29.1"))

;;; Commentary:

;; Run all tests, the test at point, the tests of a class or of a file with `kargo test', and
;; list each test's result in a buffer where RET goes to the test, `d' shows why it failed,
;; `r' runs it again and `R' runs every failed test again.
;;
;; `kargo test' filters by a substring of `package.Class.method', so a test is run by its
;; qualified name.  Several tests (a file with two classes, the failed tests) run one
;; `kargo test' after the other and their results are collected in one buffer.
;;
;; Tests are found by reading the Kotlin source: a `fun' below a `@Test' or
;; `@ParameterizedTest' annotation, in the class declared above it.  `kargo-test-pick' asks
;; `kargo test --list' instead, which compiles the tests and so knows exactly what runs.

;;; Code:

(require 'cl-lib)
(require 'subr-x)
(require 'tabulated-list)
(require 'kargo)
(require 'kargo-messages)

(defcustom kargo-test-display-results t
  "Whether to show the results buffer when tests ran.
t shows it always, `failures' only when a test failed, nil never."
  :type '(choice (const :tag "Always" t) (const :tag "When a test failed" failures) (const :tag "Never" nil))
  :group 'kargo)

(defvar kargo-test--state (make-hash-table :test 'equal)
  "A plist of each project root: `:results' (newest last), `:queue', `:extra' and `:active'.")

;;;; Finding tests in a buffer

(defun kargo-test--scan ()
  "Read the package, the classes and the tests of the current buffer.
Return a plist with `:package', `:classes', a list of (NAME LINE QUALIFIED-NAME),
and `:tests', a list of (NAME LINE CLASS) where CLASS is the entry of
`:classes' declared above the test, or nil."
  (save-excursion
    (goto-char (point-min))
    (let ((case-fold-search nil)
          package classes tests current annotated)
      (while (not (eobp))
        (let ((line (buffer-substring-no-properties (line-beginning-position) (line-end-position)))
              (row (line-number-at-pos nil t)))
          (when (string-match "^[ \t]*package[ \t]+\\([[:alnum:]_.]+\\)" line)
            (setq package (match-string 1 line)))
          (when (string-match "^[ \t]*\\(?:[[:alnum:]_@() \t]*[ \t]\\)?class[ \t]+\\([[:alnum:]_]+\\)" line)
            (let ((name (match-string 1 line)))
              (setq current (list name row (if package (concat package "." name) name)))
              (push current classes)))
          (when (string-match-p "@\\(?:Test\\|ParameterizedTest\\|RepeatedTest\\)\\b" line)
            (setq annotated t))
          (when (or (string-match "\\<fun[ \t]+`\\([^`]+\\)`" line)
                    (string-match "\\<fun[ \t]+\\([[:alnum:]_]+\\)[ \t]*(" line))
            (when annotated
              (push (list (match-string 1 line) row current) tests))
            (setq annotated nil)))
        (forward-line 1))
      (list :package package :classes (nreverse classes) :tests (nreverse tests)))))

(defun kargo-test--test-filter (test)
  "The qualified name `Class.method' of TEST, an entry of `kargo-test--scan'."
  (let ((class (nth 2 test)))
    (if class (concat (nth 2 class) "." (nth 0 test)) (nth 0 test))))

(defun kargo-test--last-before (entries line)
  "The last of ENTRIES, which are lists (NAME LINE ...), that starts at or before LINE."
  (let (found)
    (dolist (entry entries found)
      (when (<= (nth 1 entry) line) (setq found entry)))))

;;;; Running tests

(defun kargo-test--launch (root filter)
  "Run `kargo test' in ROOT for the tests that match FILTER (all of them when nil)."
  (kargo--compile root (append '("test")
                               (kargo--build-args root)
                               (plist-get (gethash root kargo-test--state) :extra)
                               (and filter (list filter)))))

(defun kargo-test--start (root filters &optional extra)
  "Run the tests that match each of FILTERS in ROOT, one `kargo test' after the other.
FILTERS is a list of strings, or (nil) for every test.  EXTRA are more arguments."
  (puthash root (list :active t :results nil :queue (cdr filters) :extra extra) kargo-test--state)
  (kargo-test--launch root (car filters)))

(defun kargo-test--finished (root subcommand messages)
  "Collect the results of a `kargo test' in ROOT, and start the next filter or report.
This is a `kargo-finished-functions' function."
  (when (equal subcommand "test")
    (let* ((state (gethash root kargo-test--state))
           (active (plist-get state :active))
           (results (append (and active (plist-get state :results))
                            (delq nil (mapcar #'kargo-messages-test-result messages))))
           (queue (and active (plist-get state :queue))))
      (puthash root (list :active (and queue t) :results results :queue (cdr queue)
                          :extra (and active (plist-get state :extra)))
               kargo-test--state)
      (if queue
          (run-at-time 0 nil #'kargo-test--launch root (car queue))
        (kargo-test--report root results)))))

(add-hook 'kargo-finished-functions #'kargo-test--finished)

(defun kargo-test--report (root results)
  "Tell how many of the test RESULTS of ROOT passed, and show them."
  (when results
    (pcase-let ((`(,passed ,failed ,ignored) (kargo-messages-counts results)))
      (message "kargo test: %d passed, %d failed, %d ignored" passed failed ignored)
      (when (or (eq kargo-test-display-results t)
                (and (eq kargo-test-display-results 'failures) (> failed 0)))
        (kargo-test--show root results)))))

;;;###autoload
(defun kargo-test (&optional extra)
  "Run every test of the current project.  A prefix argument asks for EXTRA arguments."
  (interactive (list (kargo--read-extra-arguments)))
  (kargo-test--start (kargo--root) '(nil) extra))

;;;###autoload
(defun kargo-test-at-point ()
  "Run the test function at point, the nearest one above it."
  (interactive)
  (let* ((root (kargo--root))
         (test (kargo-test--last-before (plist-get (kargo-test--scan) :tests) (line-number-at-pos nil t))))
    (unless test
      (user-error "No test at or above point"))
    (kargo-test--start root (list (kargo-test--test-filter test)))))

;;;###autoload
(defun kargo-test-class ()
  "Run the tests of the class at point, the nearest one above it."
  (interactive)
  (let* ((root (kargo--root))
         (class (kargo-test--last-before (plist-get (kargo-test--scan) :classes) (line-number-at-pos nil t))))
    (unless class
      (user-error "No class at or above point"))
    (kargo-test--start root (list (nth 2 class)))))

;;;###autoload
(defun kargo-test-file ()
  "Run the tests of every class in this file that has some."
  (interactive)
  (let* ((root (kargo--root))
         (scan (kargo-test--scan))
         (classes (delete-dups (delq nil (mapcar (lambda (test) (nth 2 (nth 2 test))) (plist-get scan :tests))))))
    (unless classes
      (user-error "No tests in this file"))
    (kargo-test--start root classes)))

(defun kargo-test--failed-filters (results)
  "The qualified names of the failed tests in RESULTS, without duplicates."
  (delete-dups
   (cl-loop for result in results
            when (equal (alist-get 'outcome result) "failed")
            collect (kargo-messages-test-title result))))

;;;###autoload
(defun kargo-test-rerun-failed ()
  "Run the tests that failed in the last test run of this project again."
  (interactive)
  (let* ((root (kargo--root))
         (filters (kargo-test--failed-filters (plist-get (gethash root kargo-test--state) :results))))
    (unless filters
      (user-error "No failed tests to run again"))
    (kargo-test--start root filters)))

;;;###autoload
(defun kargo-test-pick ()
  "Ask `kargo test --list' for the tests of the project, then run the one picked.
Listing compiles the tests, so it takes a moment."
  (interactive)
  (let* ((root (kargo--root))
         (target (kargo-target root)))
    (message "kargo: listing the tests...")
    (kargo--async
     root (append '("test" "--list" "--format" "json") (when target (list "-t" target)))
     (lambda (stdout code)
       (let ((titles (delete-dups
                      (cl-loop for message in (kargo-messages-decode stdout)
                               when (equal (alist-get 'reason message) "test-discovered")
                               collect (kargo-messages-test-title message)))))
         (if (null titles)
             (message "kargo: %s" (if (eq code 0) "no tests found"
                                    "listing the tests failed; run `kargo test --list' to see why"))
           (run-at-time 0 nil
                        (lambda ()
                          (kargo-test--start root (list (completing-read "Test: " titles nil t)))))))))))

;;;; The results buffer

(defvar-local kargo-test--root nil "The project whose results this buffer lists.")

(defvar kargo-test-results-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "RET") #'kargo-test-results-visit)
    (define-key map "d" #'kargo-test-results-details)
    (define-key map "r" #'kargo-test-results-rerun)
    (define-key map "R" #'kargo-test-rerun-failed)
    map)
  "The keymap of `kargo-test-results-mode'.")

(define-derived-mode kargo-test-results-mode tabulated-list-mode "Kargo-Tests"
  "Major mode for the results of a `kargo test' run.

\\{kargo-test-results-mode-map}"
  (setq tabulated-list-format [("Result" 7 t) ("Test" 64 t) ("Time" 8 nil)]
        tabulated-list-padding 1)
  (tabulated-list-init-header))

(defun kargo-test--entry (result)
  "The row of the test RESULT in the results buffer."
  (let ((outcome (alist-get 'outcome result))
        (duration (alist-get 'duration result)))
    (list result
          (vector (pcase outcome
                    ("passed" (propertize "pass" 'face 'success))
                    ("failed" (propertize "FAIL" 'face 'error))
                    (_ (propertize "skip" 'face 'shadow)))
                  (kargo-messages-test-title result)
                  (if duration (format "%d ms" duration) "")))))

(defun kargo-test--show (root results)
  "Show the test RESULTS of ROOT in the results buffer."
  (let ((buffer (get-buffer-create (format "*kargo tests: %s*" (kargo--project-name root)))))
    (with-current-buffer buffer
      (kargo-test-results-mode)
      (setq kargo-test--root root
            tabulated-list-entries (mapcar #'kargo-test--entry results))
      (tabulated-list-print))
    (display-buffer buffer)))

;;;###autoload
(defun kargo-test-results ()
  "Show the results of the last test run of this project."
  (interactive)
  (let* ((root (kargo--root))
         (results (plist-get (gethash root kargo-test--state) :results)))
    (unless results
      (user-error "No test results yet; run the tests first"))
    (kargo-test--show root results)))

(defun kargo-test--result-at-point ()
  "The test result on the current line of the results buffer."
  (or (tabulated-list-get-id) (user-error "No test on this line")))

(defun kargo-test--locate (root result)
  "The file and line of the source of the test RESULT in ROOT, as (FILE . LINE), or nil."
  (when-let* ((suite (alist-get 'suite result))
              (outer (car (split-string (car (last (split-string suite "\\."))) "\\$")))
              (file (or (kargo--find-source root (concat outer ".kt") suite)
                        (kargo--find-source root (concat outer ".java") suite))))
    (with-temp-buffer
      (insert-file-contents file)
      (goto-char (point-min))
      (setq case-fold-search nil)
      (cons file
            (if (re-search-forward (concat "\\<fun[ \t]+`?" (regexp-quote (alist-get 'name result)) "`?[ \t]*(")
                                   nil t)
                (line-number-at-pos (match-beginning 0) t)
              1)))))

(defun kargo-test-results-visit ()
  "Go to the source of the test on this line."
  (interactive)
  (let* ((result (kargo-test--result-at-point))
         (location (kargo-test--locate kargo-test--root result)))
    (unless location
      (user-error "Cannot find the source of %s" (kargo-messages-test-title result)))
    (find-file-other-window (car location))
    (goto-char (point-min))
    (forward-line (1- (cdr location)))))

(defun kargo-test-results-details ()
  "Show why the test on this line failed, or what it printed."
  (interactive)
  (let ((result (kargo-test--result-at-point)))
    (with-help-window "*kargo test details*"
      (princ (format "%s: %s\n\n%s\n\n%s\n"
                     (kargo-messages-test-title result)
                     (alist-get 'outcome result)
                     (or (alist-get 'message result) "")
                     (or (alist-get 'details result) ""))))))

(defun kargo-test-results-rerun ()
  "Run the test on this line again."
  (interactive)
  (let ((result (kargo-test--result-at-point)))
    (kargo-test--start kargo-test--root (list (kargo-messages-test-title result)))))

(provide 'kargo-test)
;;; kargo-test.el ends here
