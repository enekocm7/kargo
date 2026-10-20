;;; lsp-kargo.el --- lsp-mode client for Kargo  -*- lexical-binding: t; -*-

;; SPDX-License-Identifier: MIT OR Apache-2.0

;; Author: The Kargo authors
;; URL: https://github.com/enekocm7/kargo
;; Package-Requires: ((emacs "29.1") (lsp-mode "8.0"))

;;; Commentary:

;; Registers `kargo lsp' as an lsp-mode client for Kotlin files and `kargo.toml' in a Kargo
;; project:
;;
;;   (with-eval-after-load 'lsp-mode
;;     (require 'lsp-kargo))
;;
;; The client activates only in a directory with a `kargo.toml' above it, and outranks other
;; Kotlin clients there.  Which Kotlin server runs behind `kargo lsp' and check on save are
;; the `kargo-lsp-*' options of `kargo'.  Diagnostics reach Flycheck or Flymake the way
;; lsp-mode reports any server's.
;;
;; `kargo-runnables' asks the server for the `kargo/runnables' request.

;;; Code:

(require 'lsp-mode)
(require 'kargo)

(defconst lsp-kargo-source-modes '(kotlin-mode kotlin-ts-mode)
  "The Kotlin major modes the client activates in.")

(defconst lsp-kargo-manifest-modes '(conf-toml-mode toml-ts-mode toml-mode)
  "The TOML major modes the client activates in, for `kargo.toml' only.")

(defun lsp-kargo--activate-p (file-name &optional mode)
  "Whether `kargo lsp' serves the buffer of FILE-NAME in major mode MODE."
  (and file-name
       (not (file-remote-p file-name))
       (kargo-project-root (file-name-directory file-name))
       (or (apply #'provided-mode-derived-p mode lsp-kargo-source-modes)
           (and (equal (file-name-nondirectory file-name) "kargo.toml")
                (apply #'provided-mode-derived-p mode lsp-kargo-manifest-modes)))))

(dolist (entry '((kotlin-ts-mode . "kotlin") (conf-toml-mode . "toml") (toml-ts-mode . "toml")))
  (unless (assq (car entry) lsp-language-id-configuration)
    (add-to-list 'lsp-language-id-configuration entry)))

(lsp-register-client
 (make-lsp-client
  :new-connection (lsp-stdio-connection #'kargo-lsp-command)
  :activation-fn #'lsp-kargo--activate-p
  :initialization-options #'kargo-lsp-initialization-options
  :priority 2
  :server-id 'kargo))

;;;; Runnables

(defun lsp-kargo--runnable (item)
  "The runnable plist of ITEM, a `kargo/runnables' entry."
  (list :label (lsp-get item :label)
        :kind (intern (or (lsp-get item :kind) "run"))
        :args (append (lsp-get item :args) nil)
        :debug-args (and (lsp-get item :debugArgs) (append (lsp-get item :debugArgs) nil))))

(defun lsp-kargo-runnables (root)
  "The runnables the Kargo server of the current buffer lists for ROOT, or nil.
This is a `kargo-runnables-functions' function."
  (when (and (bound-and-true-p lsp-mode) (lsp-find-workspace 'kargo buffer-file-name))
    (condition-case nil
        (mapcar #'lsp-kargo--runnable
                (lsp-request "kargo/runnables"
                             (list :textDocument
                                   (list :uri (lsp--path-to-uri (or buffer-file-name (expand-file-name "kargo.toml" root)))))))
      (error nil))))

(add-hook 'kargo-runnables-functions #'lsp-kargo-runnables -50)

(provide 'lsp-kargo)
;;; lsp-kargo.el ends here
