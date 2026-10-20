;;; kargo-flymake.el --- Show Kargo build diagnostics in Flymake  -*- lexical-binding: t; -*-

;; SPDX-License-Identifier: MIT OR Apache-2.0

;; Author: The Kargo authors
;; URL: https://github.com/enekocm7/kargo
;; Package-Requires: ((emacs "29.1"))

;;; Commentary:

;; A Flymake backend for the compiler errors and warnings of the last `kargo build', `check',
;; `run' or `test': when one of those ends, its diagnostics are kept, and Flymake shows the
;; ones of each file in its buffer, with the line and column the compiler reported.  They are
;; listed by `flymake-show-buffer-diagnostics' and `flymake-show-project-diagnostics' too.
;;
;;   (add-hook 'kotlin-mode-hook #'kargo-flymake-setup)
;;
;; With `kargo-flymake-check-on-save', saving a file also runs `kargo check' in the
;; background.  The language server does that already with Kargo's own Kotlin server
;; (check on save), so use this backend either without a language server or with
;; `kargo-lsp-check-on-save' set to nil.

;;; Code:

(require 'cl-lib)
(require 'flymake)
(require 'kargo)
(require 'kargo-messages)

(defcustom kargo-flymake-check-on-save nil
  "Whether saving a file of a Kargo project runs `kargo check' to refresh the diagnostics.
Set it before `kargo-flymake-setup' runs."
  :type 'boolean
  :group 'kargo)

(defvar kargo-flymake--diagnostics (make-hash-table :test 'equal)
  "For each project root, a hash table from a file to its diagnostics.
A diagnostic is a list (LINE COLUMN SEVERITY TEXT).")

(defvar kargo-flymake--process nil "The background `kargo check' of `kargo-flymake-check'.")

(defun kargo-flymake--store (root messages)
  "Keep the compiler diagnostics in MESSAGES as the ones of the project in ROOT."
  (let ((table (make-hash-table :test 'equal)))
    (dolist (diagnostic (kargo-messages-diagnostics messages))
      (pcase-let ((`(,file . ,rest) diagnostic))
        (cl-pushnew rest (gethash (file-truename file) table) :test #'equal)))
    (puthash root table kargo-flymake--diagnostics)
    (kargo-flymake--refresh root)))

(defun kargo-flymake--refresh (root)
  "Ask Flymake to check again in the buffers of the files under ROOT."
  (dolist (buffer (buffer-list))
    (with-current-buffer buffer
      (when (and (bound-and-true-p flymake-mode)
                 buffer-file-name
                 (file-in-directory-p buffer-file-name root))
        (flymake-start)))))

(defun kargo-flymake--finished (root subcommand messages)
  "Keep the diagnostics of a command that ended in ROOT.
This is a `kargo-finished-functions' function."
  (when (member subcommand '("build" "check" "run" "test"))
    (kargo-flymake--store root messages)))

(add-hook 'kargo-finished-functions #'kargo-flymake--finished)

(defun kargo-flymake (report-fn &rest _args)
  "A Flymake backend that reports the diagnostics of the last Kargo command.
REPORT-FN is the function Flymake gives a backend."
  (let* ((root (and buffer-file-name (kargo-project-root)))
         (table (and root (gethash root kargo-flymake--diagnostics)))
         (source (current-buffer)))
    (funcall
     report-fn
     (when table
       (mapcar (lambda (diagnostic)
                 (pcase-let ((`(,line ,column ,severity ,text) diagnostic))
                   (flymake-make-diagnostic source (cons line column) nil
                                            (pcase severity ('error :error) ('warning :warning) (_ :note))
                                            text)))
               (gethash (file-truename buffer-file-name) table))))))

(defun kargo-flymake-check ()
  "Run `kargo check' in the background and show its diagnostics in Flymake."
  (interactive)
  (let ((root (kargo--root)))
    (when (process-live-p kargo-flymake--process)
      (delete-process kargo-flymake--process))
    (setq kargo-flymake--process
          (kargo--async root (append '("check" "--keep-going" "--message-format" "json")
                                     (when kargo-check-tests '("--tests"))
                                     (kargo--build-args root))
                        (lambda (stdout _code)
                          (kargo-flymake--store root (kargo-messages-decode stdout)))))))

;;;###autoload
(defun kargo-flymake-setup ()
  "Add the Kargo backend to Flymake in the current buffer, in a Kargo project."
  (interactive)
  (when (and buffer-file-name (kargo-project-root))
    (add-hook 'flymake-diagnostic-functions #'kargo-flymake nil t)
    (when kargo-flymake-check-on-save
      (add-hook 'after-save-hook #'kargo-flymake-check nil t)
      ;; Diagnostics come from saving, so Flymake needn't check after every change.
      (setq-local flymake-no-changes-timeout nil))
    (flymake-mode 1)))

(provide 'kargo-flymake)
;;; kargo-flymake.el ends here
