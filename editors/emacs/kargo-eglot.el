;;; kargo-eglot.el --- Eglot support for Kargo  -*- lexical-binding: t; -*-

;; SPDX-License-Identifier: MIT OR Apache-2.0

;; Author: The Kargo authors
;; URL: https://github.com/enekocm7/kargo
;; Package-Requires: ((emacs "29.1"))

;;; Commentary:

;; Run `kargo lsp' from Eglot for Kotlin files and `kargo.toml':
;;
;;   (with-eval-after-load 'eglot
;;     (require 'kargo-eglot)
;;     (kargo-eglot-setup))
;;
;; `kargo-eglot-setup' adds `kargo lsp' to `eglot-server-programs' for `kotlin-mode',
;; `kotlin-ts-mode' and the TOML modes, makes `project.el' find the root through
;; `kargo.toml', and (with `kargo-eglot-auto-start') starts Eglot when you open a Kotlin
;; file or a `kargo.toml' of a Kargo project.  Which Kotlin server runs behind `kargo lsp',
;; and check on save, are the `kargo-lsp-*' options of `kargo'.
;;
;; It also answers `kargo-runnables' with the `kargo/runnables' request.

;;; Code:

(require 'cl-lib)
(require 'eglot)
(require 'jsonrpc)
(require 'kargo)

(defcustom kargo-eglot-auto-start nil
  "Whether to start Eglot in the Kotlin files and `kargo.toml' of a Kargo project.
Set it before calling `kargo-eglot-setup'."
  :type 'boolean
  :group 'kargo)

(defcustom kargo-eglot-modes '(kotlin-mode kotlin-ts-mode conf-toml-mode toml-ts-mode)
  "The major modes `kargo-eglot-setup' makes Eglot serve with `kargo lsp'.
Leave out the TOML modes to use another TOML server, such as Taplo.  Set it
before calling `kargo-eglot-setup'."
  :type '(repeat symbol)
  :group 'kargo)

(declare-function eglot-path-to-uri "eglot")
(declare-function eglot--path-to-uri "eglot")

(defun kargo-eglot-contact (&optional _interactive _project)
  "The contact of the Kargo language server for `eglot-server-programs'.
It is `kargo lsp' with the options of `kargo-lsp-command' and
`kargo-lsp-initialization-options'."
  (let ((options (kargo-lsp-initialization-options)))
    (append (kargo-lsp-command)
            (when options (list :initializationOptions options)))))

(defun kargo-eglot--manifest-p ()
  "Whether the buffer is a `kargo.toml'."
  (and buffer-file-name (equal (file-name-nondirectory buffer-file-name) "kargo.toml")))

(defun kargo-eglot-ensure ()
  "Start Eglot in the current buffer when it belongs to a Kargo project.
Kotlin files of a project and its `kargo.toml' qualify; other TOML files don't."
  (when (and buffer-file-name
             (kargo-project-root)
             (or (apply #'derived-mode-p '(kotlin-mode kotlin-ts-mode)) (kargo-eglot--manifest-p)))
    (eglot-ensure)))

;;;###autoload
(defun kargo-eglot-setup ()
  "Make Eglot run `kargo lsp' for Kotlin files and `kargo.toml'."
  (interactive)
  (kargo-setup-project)
  (setq eglot-server-programs
        (cons (cons kargo-eglot-modes #'kargo-eglot-contact)
              (cl-remove-if (lambda (entry) (eq (cdr entry) #'kargo-eglot-contact)) eglot-server-programs)))
  (when kargo-eglot-auto-start
    (dolist (hook '(kotlin-mode-hook kotlin-ts-mode-hook conf-toml-mode-hook toml-ts-mode-hook))
      (add-hook hook #'kargo-eglot-ensure))))

;;;; Runnables

(defun kargo-eglot--uri (file)
  "The `file:' URI of FILE."
  (if (fboundp 'eglot-path-to-uri) (eglot-path-to-uri file) (eglot--path-to-uri file)))

(defun kargo-eglot--runnable (item)
  "The runnable plist of ITEM, a `kargo/runnables' entry."
  (list :label (plist-get item :label)
        :kind (intern (or (plist-get item :kind) "run"))
        :args (append (plist-get item :args) nil)
        :debug-args (and (plist-get item :debugArgs) (append (plist-get item :debugArgs) nil))))

(defun kargo-eglot-runnables (root)
  "The runnables the Kargo server of the current buffer lists for ROOT, or nil.
This is a `kargo-runnables-functions' function."
  (when-let* ((server (and (bound-and-true-p eglot--managed-mode) (eglot-current-server)))
              ((plist-get (plist-get (plist-get (eglot-server-capabilities server) :experimental) :kargo)
                          :runnables)))
    (condition-case nil
        (mapcar #'kargo-eglot--runnable
                (jsonrpc-request server :kargo/runnables
                                 (list :textDocument
                                       (list :uri (kargo-eglot--uri
                                                   (or buffer-file-name (expand-file-name "kargo.toml" root)))))))
      (error nil))))

(add-hook 'kargo-runnables-functions #'kargo-eglot-runnables -50)

(provide 'kargo-eglot)
;;; kargo-eglot.el ends here
