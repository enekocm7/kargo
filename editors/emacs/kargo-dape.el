;;; kargo-dape.el --- Debug Kargo programs with Dape  -*- lexical-binding: t; -*-

;; SPDX-License-Identifier: MIT OR Apache-2.0

;; Author: The Kargo authors
;; URL: https://github.com/enekocm7/kargo
;; Package-Requires: ((emacs "29.1") (dape "0.20"))

;;; Commentary:

;; Debug JVM programs and tests with Dape through `kargo dap'.  `kargo dap' builds the
;; project, starts the program with `kargo run --debug' (or `kargo test --debug'), and
;; attaches once the JVM waits for a debugger, so a breakpoint on the first line of `main'
;; is hit.
;;
;;   (with-eval-after-load 'dape
;;     (require 'kargo-dape)
;;     (kargo-dape-setup))
;;
;; After that `M-x dape' offers the configurations `kargo' (the program), `kargo-test'
;; (the tests) and `kargo-attach' (a JVM started with `kargo run --debug').
;; `kargo-dape-run', `kargo-dape-test-at-point' and `kargo-dape-attach' start them directly.

;;; Code:

(require 'cl-lib)
(require 'dape)
(require 'kargo)
(require 'kargo-test)

(defcustom kargo-dape-attach-port 5005
  "The port `kargo-dape-attach' connects to: the one `kargo run --debug' listens on."
  :type 'integer
  :group 'kargo)

(defun kargo-dape-cwd ()
  "The root of the Kargo project of the current buffer, where `kargo dap' must run."
  (directory-file-name (or (kargo-project-root) default-directory)))

(defun kargo-dape--configs ()
  "The Dape configurations for Kargo, as an alist from a name to a plist."
  `((kargo
     modes (kotlin-mode kotlin-ts-mode)
     command ,(or (executable-find kargo-executable) kargo-executable)
     command-args ("dap")
     command-cwd kargo-dape-cwd
     :request "launch"
     :type "kotlin"
     :args ["run"])
    (kargo-test
     modes (kotlin-mode kotlin-ts-mode)
     command ,(or (executable-find kargo-executable) kargo-executable)
     command-args ("dap")
     command-cwd kargo-dape-cwd
     :request "launch"
     :type "kotlin"
     :args ["test"])
    (kargo-attach
     modes (kotlin-mode kotlin-ts-mode)
     command ,(or (executable-find kargo-executable) kargo-executable)
     command-args ("dap")
     command-cwd kargo-dape-cwd
     :request "attach"
     :type "kotlin"
     :hostName "127.0.0.1"
     :port ,kargo-dape-attach-port)))

;;;###autoload
(defun kargo-dape-setup ()
  "Add the `kargo', `kargo-test' and `kargo-attach' configurations to `dape-configs'."
  (interactive)
  (dolist (config (kargo-dape--configs))
    (setf (alist-get (car config) dape-configs) (cdr config))))

(defun kargo-dape--start (name &rest overrides)
  "Start the Dape configuration NAME with the plist OVERRIDES put over it."
  (kargo-dape-setup)
  (let ((config (copy-sequence (alist-get name dape-configs))))
    (cl-loop for (key value) on overrides by #'cddr
             do (setq config (plist-put config key value)))
    (dape config)))

;;;###autoload
(defun kargo-dape-run (&optional arguments)
  "Debug the program of the current project.
A prefix argument reads ARGUMENTS, the arguments of `kargo run', such as
\"--bin server\"."
  (interactive (list (when current-prefix-arg (read-string "kargo run arguments: "))))
  (kargo--root)
  (kargo-dape--start 'kargo :args (vconcat '("run") (split-string-and-unquote (or arguments "")))))

;;;###autoload
(defun kargo-dape-test-at-point ()
  "Debug the test at point, or every test of the project when point is not in one."
  (interactive)
  (kargo--root)
  (let ((test (kargo-test--last-before (plist-get (kargo-test--scan) :tests) (line-number-at-pos nil t))))
    (kargo-dape--start 'kargo-test :args (vconcat '("test") (when test (list (kargo-test--test-filter test)))))))

;;;###autoload
(defun kargo-dape-attach ()
  "Attach to a JVM started with `kargo run --debug' on `kargo-dape-attach-port'."
  (interactive)
  (kargo--root)
  (kargo-dape--start 'kargo-attach))

(provide 'kargo-dape)
;;; kargo-dape.el ends here
