;;; kargo.el --- Build Kotlin projects with Kargo  -*- lexical-binding: t; -*-

;; SPDX-License-Identifier: MIT OR Apache-2.0

;; Author: The Kargo authors
;; URL: https://github.com/enekocm7/kargo
;; Version: 0.1.0
;; Package-Requires: ((emacs "29.1"))
;; Keywords: tools, languages, processes

;;; Commentary:

;; Kargo is Cargo for Kotlin: one `kargo.toml' and one CLI that build Kotlin projects for the
;; JVM, WebAssembly, Kotlin/Native and Android.  This package runs the Kargo commands from
;; Emacs:
;;
;;   M-x kargo-build, kargo-check, kargo-run, kargo-test, kargo-clean,
;;   kargo-add, kargo-remove, kargo-update, kargo-fetch, kargo-task,
;;   kargo-runnables, kargo-set-target, kargo-command
;;
;; Each one runs in a `compilation-mode' buffer, so `next-error' (M-g n) jumps to compiler
;; errors, warnings and the frames of a failed test.  Kargo prints its messages as JSON
;; (`--message-format json'), and this package turns them into compiler-style lines with
;; absolute paths.
;;
;; `kargo-mode' binds the commands under C-c k in Kotlin and `kargo.toml' buffers of a Kargo
;; project; `global-kargo-mode' turns it on for all of them.  The other files of the package
;; set up the rest:
;;
;;   kargo-eglot     the language server `kargo lsp' for Eglot
;;   kargo-lsp       the same for lsp-mode, `lsp-kargo'
;;   kargo-test      run a test, a class or a file, and the results buffer
;;   kargo-flymake   build diagnostics in Flymake
;;   kargo-dape      debugging with Dape (`kargo dap')
;;   kargo-transient a menu of all commands
;;
;; See https://enekocm7.github.io/kargo/guides/editors/#emacs.

;;; Code:

(require 'cl-lib)
(require 'compile)
(require 'project)
(require 'subr-x)
(eval-when-compile (require 'rx))
(require 'kargo-messages)

(defgroup kargo nil
  "Build Kotlin projects with Kargo."
  :group 'tools
  :prefix "kargo-"
  :link '(url-link "https://enekocm7.github.io/kargo/guides/editors/#emacs"))

(defcustom kargo-executable "kargo"
  "The Kargo executable: a name looked up in `exec-path', or a file name."
  :type 'string)

(defcustom kargo-check-tests t
  "Whether `kargo-check' checks the test sources too."
  :type 'boolean)

(defcustom kargo-manifest-modes '(conf-toml-mode toml-ts-mode toml-mode)
  "Major modes in which `global-kargo-mode' turns on for `kargo.toml'."
  :type '(repeat symbol))

(defcustom kargo-source-modes '(kotlin-mode kotlin-ts-mode java-mode java-ts-mode)
  "Major modes in which `global-kargo-mode' turns on for sources."
  :type '(repeat symbol))

(defvar kargo-subcommands
  '("build" "check" "run" "test" "clean" "add" "remove" "update" "upgrade" "fetch" "tree" "outdated"
    "task" "target" "fmt" "lint" "doc" "package" "publish" "new" "init" "info" "metadata" "toolchain")
  "The `kargo' subcommands `kargo-command' offers.")

(defvar kargo--command-history nil "History of `kargo-command'.")
(defvar kargo--arguments-history nil "History of the extra arguments of a command.")

;;;; The language server

(defcustom kargo-lsp-server nil
  "The Kotlin server that `kargo lsp' runs.
nil is Kargo's own server, the default.  \"kotlin-lsp\" is JetBrains' Kotlin
LSP, \"kls\" fwcd's kotlin-language-server, and \"none\" starts no Kotlin
server, so only `kargo.toml' support remains."
  :type '(choice (const :tag "Kargo's own (default)" nil)
                 (const :tag "JetBrains' Kotlin LSP" "kotlin-lsp")
                 (const :tag "kotlin-language-server" "kls")
                 (const :tag "No Kotlin server" "none"))
  :group 'kargo)

(defcustom kargo-lsp-server-path nil
  "A Kotlin server executable to run instead of the one Kargo downloads, or nil."
  :type '(choice (const nil) file)
  :group 'kargo)

(defcustom kargo-lsp-klib-targets nil
  "Whether `kargo lsp' also describes Wasm and Native code to the Kotlin LSP."
  :type 'boolean
  :group 'kargo)

(defcustom kargo-lsp-check-on-save 'default
  "Whether `kargo lsp' runs `kargo check' when a Kotlin file is saved.
`default' leaves it to the server: on with Kargo's own Kotlin server, off with
the others.  Errors from the check appear with the server's diagnostics."
  :type '(choice (const :tag "Server default" default) (const :tag "On" t) (const :tag "Off" nil))
  :group 'kargo)

(defcustom kargo-lsp-check-targets nil
  "The targets check on save compiles, such as (\"wasm\" \"native\"), or nil for all."
  :type '(repeat string)
  :group 'kargo)

(defun kargo-lsp-command ()
  "The command line of the language server, a list of strings.
It follows `kargo-lsp-server', `kargo-lsp-server-path' and `kargo-lsp-klib-targets'."
  (append (list (kargo--executable) "lsp")
          (when kargo-lsp-server (list "--server" kargo-lsp-server))
          (when kargo-lsp-server-path (list "--server-path" (expand-file-name kargo-lsp-server-path)))
          (when kargo-lsp-klib-targets '("--klib-targets"))))

(defun kargo-lsp-initialization-options ()
  "The `initializationOptions' of the language server, a plist, or nil."
  (append (unless (eq kargo-lsp-check-on-save 'default)
            (list :checkOnSave (if kargo-lsp-check-on-save t :json-false)))
          (when kargo-lsp-check-targets
            (list :checkTargets (vconcat kargo-lsp-check-targets)))))

;;;; The project

(defun kargo--executable ()
  "The Kargo executable, or a `user-error' that says how to get it."
  (or (executable-find kargo-executable)
      (user-error "Kargo is not installed: no `%s' in `exec-path'.  Install it from https://enekocm7.github.io/kargo/getting-started/installation/ or set `kargo-executable'"
                  kargo-executable)))

(defun kargo-project-root (&optional directory)
  "The directory of the nearest `kargo.toml' above DIRECTORY, or nil.
DIRECTORY defaults to `default-directory'."
  (when-let* ((dir (locate-dominating-file (or directory default-directory) "kargo.toml")))
    (file-name-as-directory (expand-file-name dir))))

(defun kargo--root ()
  "The root of the Kargo project of the current buffer, or a `user-error'."
  (or (kargo-project-root)
      (user-error "No kargo.toml found in %s or above" (abbreviate-file-name default-directory))))

(defun kargo--project-name (root)
  "A short name for the project in ROOT."
  (file-name-nondirectory (directory-file-name root)))

;;;; Calling Kargo

(defun kargo--call-json (root &rest args)
  "Run `kargo ARGS' in ROOT and return its output parsed as JSON, or nil if it fails."
  (let ((default-directory root))
    (with-temp-buffer
      (when (eq 0 (apply #'call-process (kargo--executable) nil (list (current-buffer) nil) nil args))
        (goto-char (point-min))
        (ignore-errors (kargo-messages-json (buffer-string)))))))

(defun kargo--async (root args callback)
  "Run `kargo ARGS' in ROOT without a buffer and call CALLBACK with STDOUT and the exit code.
STDOUT also holds the status lines Kargo prints to stderr, so read it line by
line with `kargo-messages-decode'.  Return the process."
  (let* ((default-directory root)
         (output (generate-new-buffer " *kargo output*")))
    (make-process
     :name "kargo" :buffer output :noquery t :connection-type 'pipe
     :command (cons (kargo--executable) args)
     :sentinel (lambda (process _event)
                 (unless (process-live-p process)
                   (let ((stdout (with-current-buffer output (buffer-string)))
                         (code (process-exit-status process)))
                     (kill-buffer output)
                     (funcall callback stdout code)))))))

(defun kargo--metadata (root)
  "The output of `kargo metadata --no-deps' in ROOT, or nil."
  (kargo--call-json root "metadata" "--no-deps"))

(defun kargo--package (root)
  "The package of ROOT in `kargo metadata', an alist, or nil."
  (when-let* ((metadata (kargo--metadata root)))
    (let ((packages (alist-get 'packages metadata)))
      (or (cl-find-if (lambda (package)
                        (and (alist-get 'root package)
                             (file-equal-p (alist-get 'root package) root)))
                      packages)
          (cl-find-if (lambda (package) (alist-get 'member package)) packages)
          (car packages)))))

(defun kargo--package-targets (package)
  "The names of the enabled targets of PACKAGE, from `kargo metadata'."
  (mapcar (lambda (target) (alist-get 'kind target)) (alist-get 'targets package)))

;;;; Targets and profile

(defvar kargo--targets (make-hash-table :test 'equal)
  "The active target of each project root, as a string.  Missing means every target.")

(defvar kargo--release (make-hash-table :test 'equal)
  "Whether each project root builds with the release profile.")

(defun kargo-target (&optional root)
  "The active target of the project in ROOT (default: the current one), or nil for all."
  (gethash (or root (kargo-project-root)) kargo--targets))

(defun kargo--build-args (root)
  "The target and profile options for the project in ROOT."
  (append (when-let* ((target (gethash root kargo--targets))) (list "-t" target))
          (when (gethash root kargo--release) (list "--release"))))

(defun kargo-set-target (target)
  "Make TARGET the active target of the current project.
Build, check, run and test then pass `-t TARGET'.  The choice \"all\" builds
every enabled target again."
  (interactive
   (let* ((root (kargo--root))
          (targets (or (kargo--package-targets (kargo--package root)) '("jvm" "wasm" "native" "android"))))
     (list (completing-read (format "Active target (now %s): " (or (kargo-target root) "all"))
                            (cons "all" targets) nil t nil nil "all"))))
  (let ((root (kargo--root)))
    (if (equal target "all")
        (remhash root kargo--targets)
      (puthash root target kargo--targets))
    (force-mode-line-update t)
    (message "kargo: %s" (if (equal target "all") "building every target" (format "target is %s" target)))))

(defun kargo-toggle-release ()
  "Switch the current project between the dev and the release profile."
  (interactive)
  (let ((root (kargo--root)))
    (puthash root (not (gethash root kargo--release)) kargo--release)
    (force-mode-line-update t)
    (message "kargo: %s profile" (if (gethash root kargo--release) "release" "dev"))))

;;;; Sources of stack frames

(defvar kargo--source-index nil
  "A hash table from a root to a hash table of file names to their absolute paths.")

(defun kargo--source-files (root)
  "A hash table of the Kotlin and Java files under ROOT: base name to a list of paths."
  (unless kargo--source-index
    (setq kargo--source-index (make-hash-table :test 'equal)))
  (or (gethash root kargo--source-index)
      (let ((table (make-hash-table :test 'equal)))
        (dolist (file (directory-files-recursively
                       root "\\.\\(?:kt\\|kts\\|java\\)\\'" nil
                       (lambda (dir) (not (member (file-name-nondirectory dir) '("target" ".git" "node_modules" ".gradle"))))))
          (push file (gethash (file-name-nondirectory file) table)))
        (puthash root table kargo--source-index))))

(defun kargo--find-source (root file class)
  "The absolute path of the source FILE in ROOT, or nil.
CLASS, a qualified class name, picks among files that share the name: the one
in the directory of the class's package wins."
  (let ((paths (gethash file (kargo--source-files root)))
        (package (replace-regexp-in-string "\\.?[^.]*\\'" "" (or class ""))))
    (or (and (cdr paths) (not (string-empty-p package))
             (cl-find-if (lambda (path)
                           (string-match-p (regexp-quote (concat "/" (string-replace "." "/" package) "/")) path))
                         paths))
        (car paths))))

(defun kargo--stack-frames (root details)
  "The lines of the stack trace DETAILS with the frames in ROOT's sources as links.
The first such frame is an error, which `next-error' stops at; the others are notes."
  (let ((first t))
    (mapcar
     (lambda (line)
       (let ((line (string-trim line)))
         (if (string-match "\\`at \\([^ (]+\\)(\\([^():]+\\.\\(?:kt\\|java\\)\\):\\([0-9]+\\))" line)
             (let* ((qualified (match-string 1 line))
                    (file (match-string 2 line))
                    (number (match-string 3 line))
                    (class (replace-regexp-in-string "\\.[^.]+\\'" "" qualified))
                    (path (kargo--find-source root file class)))
               (if path
                   (prog1 (format "    %s:%s: %s: %s" path number (if first "error" "info") line)
                     (setq first nil))
                 (concat "    " line)))
           (concat "    " line))))
     (split-string (or details "") "\n" t))))

;;;; The compilation buffer

(defvar-local kargo--filter-pos nil
  "Where the output that `kargo--filter' has not looked at yet begins.")

(defvar-local kargo--messages nil
  "The JSON messages of the command in this buffer, newest first.")

(defvar-local kargo--subcommand nil
  "The Kargo subcommand that runs in this compilation buffer.")

(defvar kargo-finished-functions nil
  "Functions called when a Kargo command ends, with ROOT, SUBCOMMAND and MESSAGES.
MESSAGES are the decoded JSON messages of the command, oldest first.  They run
in the compilation buffer.")

(defun kargo--render (message root)
  "The text that shows the JSON MESSAGE in the compilation buffer, or nil for nothing.
ROOT is the project, which stack frames are looked up in."
  (pcase (alist-get 'reason message)
    ("compiler-message" (kargo-messages-format-compiler message))
    ("test-finished"
     (let ((result (kargo-messages-test-result message)))
       (kargo-messages-format-test result (and (alist-get 'details result)
                                               (kargo--stack-frames root (alist-get 'details result))))))
    ("test-summary" (kargo-messages-format-summary message))
    (_ nil)))

(defun kargo--filter ()
  "Replace the JSON lines Kargo printed since the last call with compiler-style lines.
Lines that are not messages, such as status lines, stay as they are.  A line
that is not complete yet waits for the next call."
  (let* ((inhibit-read-only t)
         (begin (min (or kargo--filter-pos (marker-position compilation-filter-start)) (point)))
         (start begin)
         (end (copy-marker (point) t))
         (root default-directory))
    (goto-char start)
    (while (search-forward "\n" end t)
      (when-let* ((message (kargo-messages-parse (buffer-substring-no-properties start (1- (point))))))
        (push message kargo--messages)
        (let ((text (kargo--render message root)))
          (delete-region start (point))
          ;; Before markers, so that the process mark ends up after the text.
          (when text (insert-before-markers text "\n"))))
      (setq start (point)))
    (goto-char end)
    (setq kargo--filter-pos start)
    ;; Compilation mode looks for errors from here, which must include the new text.
    (when (markerp compilation-filter-start)
      (set-marker compilation-filter-start begin))
    (set-marker end nil)))

(defun kargo--revert-files (root)
  "Reload the unmodified buffers of `kargo.toml' and `kargo.lock' files under ROOT."
  (dolist (buffer (buffer-list))
    (with-current-buffer buffer
      (when (and buffer-file-name
                 (member (file-name-nondirectory buffer-file-name) '("kargo.toml" "kargo.lock"))
                 (file-in-directory-p buffer-file-name root)
                 (not (buffer-modified-p))
                 (not (verify-visited-file-modtime buffer)))
        (revert-buffer :ignore-auto :noconfirm)))))

(defun kargo--finished (buffer _message)
  "Tell `kargo-finished-functions' that the command in the compilation BUFFER ended."
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (let ((root default-directory))
        (when (equal kargo--subcommand "test")
          (require 'kargo-test))
        (when (member kargo--subcommand '("add" "remove" "update" "upgrade" "target" "fetch"))
          (kargo--revert-files root))
        (run-hook-with-args 'kargo-finished-functions root kargo--subcommand (reverse kargo--messages))))))

(define-compilation-mode kargo-compilation-mode "Kargo"
  "Compilation mode for the output of the `kargo' commands."
  (setq-local compilation-error-regexp-alist '(kargo))
  (add-hook 'compilation-filter-hook #'kargo--filter nil t)
  (add-hook 'compilation-finish-functions #'kargo--finished nil t))

(add-to-list
 'compilation-error-regexp-alist-alist
 `(kargo
   ,(rx bol (* (any " \t"))
        (group (? alpha ":") (not (any space ":\n")) (* (not (any ":\n"))))
        ":" (group (+ digit)) ":" (? (group (+ digit)) ":")
        " " (or (group "warning") (group "info") "error") ":")
   1 2 3 (4 . 5)))

(defconst kargo--json-subcommands '("build" "check" "run" "test")
  "The subcommands whose output is read as JSON messages.")

(defun kargo--compile (root args)
  "Run `kargo ARGS' in ROOT in a compilation buffer, and return the buffer.
For the subcommands that report compiler messages, Kargo is asked to print them
as JSON, which the buffer turns into links."
  (let* ((default-directory root)
         (subcommand (car args))
         (argv (if (member subcommand kargo--json-subcommands)
                   (append (list subcommand "--message-format" "json") (cdr args))
                 args))
         (command (mapconcat #'shell-quote-argument (cons (kargo--executable) argv) " "))
         (buffer-name (format "*kargo %s: %s*" subcommand (kargo--project-name root))))
    (save-some-buffers (not compilation-ask-about-save))
    (when kargo--source-index (remhash root kargo--source-index))
    (let ((buffer (compilation-start command 'kargo-compilation-mode (lambda (_mode) buffer-name))))
      (with-current-buffer buffer (setq kargo--subcommand subcommand))
      buffer)))

(defun kargo--read-extra-arguments ()
  "Read extra arguments for a command when the prefix argument is set, else nil."
  (when current-prefix-arg
    (split-string-and-unquote
     (read-string "Extra kargo arguments: " nil 'kargo--arguments-history))))

;;;; Commands

;;;###autoload
(defun kargo-build (&optional extra)
  "Run `kargo build' in the current project.  Interactively, a prefix argument asks for EXTRA arguments."
  (interactive (list (kargo--read-extra-arguments)))
  (let ((root (kargo--root)))
    (kargo--compile root (append '("build") (kargo--build-args root) extra))))

;;;###autoload
(defun kargo-check (&optional extra)
  "Run `kargo check', which type-checks without linking.  A prefix argument asks for EXTRA arguments."
  (interactive (list (kargo--read-extra-arguments)))
  (let ((root (kargo--root)))
    (kargo--compile root (append '("check") (when kargo-check-tests '("--tests")) (kargo--build-args root) extra))))

;;;###autoload
(defun kargo-run (&optional extra)
  "Run `kargo run'.  A prefix argument asks for EXTRA arguments, such as `--bin server'."
  (interactive (list (kargo--read-extra-arguments)))
  (let ((root (kargo--root)))
    (kargo--compile root (append '("run") (kargo--build-args root) extra))))

;;;###autoload
(defun kargo-clean (&optional extra)
  "Run `kargo clean'.  A prefix argument asks for EXTRA arguments."
  (interactive (list (kargo--read-extra-arguments)))
  (kargo--compile (kargo--root) (append '("clean") extra)))

;;;###autoload
(defun kargo-add (coordinates &optional dev)
  "Add the dependencies COORDINATES, a string of `group:artifact[:version]', to `kargo.toml'.
With a prefix argument DEV, add them to `[dev-dependencies]'."
  (interactive (list (read-string "Add dependencies (group:artifact[:version] ...): ") current-prefix-arg))
  (kargo--compile (kargo--root) (append '("add") (when dev '("--dev")) (split-string-and-unquote coordinates))))

(defun kargo--dependency-names (root)
  "The names of the dependencies `kargo info' lists for ROOT."
  (let ((info (kargo--call-json root "info" "--format" "json")))
    (mapcar (lambda (dependency) (alist-get 'name dependency))
            (append (alist-get 'dependencies info) (alist-get 'dev-dependencies info)))))

;;;###autoload
(defun kargo-remove (name)
  "Remove the dependency NAME from `kargo.toml'."
  (interactive
   (list (completing-read "Remove dependency: " (kargo--dependency-names (kargo--root)) nil nil)))
  (kargo--compile (kargo--root) (list "remove" name)))

;;;###autoload
(defun kargo-update (&optional extra)
  "Run `kargo update', which locks the newest versions.  A prefix argument asks for EXTRA arguments."
  (interactive (list (kargo--read-extra-arguments)))
  (kargo--compile (kargo--root) (append '("update") extra)))

;;;###autoload
(defun kargo-fetch (&optional sources)
  "Run `kargo fetch', which downloads the dependencies.
With a prefix argument SOURCES, also download their sources jars, so that
go to definition reaches library code."
  (interactive "P")
  (kargo--compile (kargo--root) (append '("fetch") (when sources '("--sources")))))

;;;###autoload
(defun kargo-tree ()
  "Run `kargo tree', which prints the dependency graph."
  (interactive)
  (kargo--compile (kargo--root) '("tree")))

(defun kargo--task-names (root)
  "The names of the tasks defined for the project in ROOT."
  (mapcar (lambda (task) (alist-get 'name task))
          (alist-get 'tasks (kargo--package root))))

;;;###autoload
(defun kargo-task (name)
  "Run the task NAME of `kargo.toml'."
  (interactive
   (list (let ((tasks (kargo--task-names (kargo--root))))
           (if tasks
               (completing-read "Task: " tasks nil nil)
             (read-string "Task: ")))))
  (kargo--compile (kargo--root) (list "task" name)))

;;;###autoload
(defun kargo-target-add (target)
  "Enable TARGET (jvm, wasm, native or android) in the current project."
  (interactive (list (completing-read "Add target: " '("jvm" "wasm" "native" "android") nil t)))
  (kargo--compile (kargo--root) (list "target" "add" target)))

;;;###autoload
(defun kargo-command (arguments)
  "Run `kargo ARGUMENTS' in the current project.  ARGUMENTS is a string, such as \"fmt --check\"."
  (interactive
   (list (completing-read "kargo " kargo-subcommands nil nil nil 'kargo--command-history)))
  (kargo--compile (kargo--root) (split-string-and-unquote arguments)))

;;;; Runnables

(defvar kargo-runnables-functions nil
  "Functions that list the runnables of a project, tried until one returns a list.
Each is called with the project root and returns runnables as plists with the
keys `:label', `:kind' (`run', `test' or `task'), `:args' (the Kargo
arguments, as a list of strings) and `:debug-args', or nil.  The language
server clients add a function that asks `kargo lsp' (the `kargo/runnables'
request); the last one reads `kargo metadata'.")

(defun kargo--runnables-from-metadata (root)
  "The runnables of the project in ROOT, built from `kargo metadata'."
  (let* ((package (kargo--package root))
         (name (alist-get 'name package))
         (targets (kargo--package-targets package))
         (jvm-debug (lambda (args) (append args '("--debug"))))
         items)
    (cl-flet ((add (label kind args &optional debug)
                (push (list :label label :kind kind :args args :debug-args (and debug (funcall jvm-debug args))) items)))
      (dolist (target targets)
        (add (format "Run %s (%s)" name target) 'run (list "run" "-t" target) (equal target "jvm")))
      (dolist (bin (alist-get 'bins package))
        (add (format "Run bin %s" (alist-get 'name bin)) 'run (list "run" "--bin" (alist-get 'name bin)) t))
      (dolist (example (alist-get 'examples package))
        (let ((example (if (stringp example) example (alist-get 'name example))))
          (add (format "Run example %s" example) 'run (list "run" "--example" example) t)))
      (dolist (target targets)
        (add (format "Test %s (%s)" name target) 'test (list "test" "-t" target) (equal target "jvm")))
      (dolist (task (alist-get 'tasks package))
        (add (format "Task %s" (alist-get 'name task)) 'task (list "task" (alist-get 'name task)))))
    (nreverse items)))

(add-hook 'kargo-runnables-functions #'kargo--runnables-from-metadata 90)

;;;###autoload
(defun kargo-runnables ()
  "Pick one of the programs, tests and tasks of the current package and run it."
  (interactive)
  (let* ((root (kargo--root))
         (items (run-hook-with-args-until-success 'kargo-runnables-functions root)))
    (unless items
      (user-error "No runnables found for %s" (abbreviate-file-name root)))
    (let* ((labels (mapcar (lambda (item) (cons (plist-get item :label) item)) items))
           (choice (completing-read "Kargo: " (mapcar #'car labels) nil t)))
      (kargo--compile root (plist-get (cdr (assoc choice labels)) :args)))))

;;;; The minor mode

(defvar kargo-command-map
  (let ((map (make-sparse-keymap)))
    (define-key map "b" #'kargo-build)
    (define-key map "c" #'kargo-check)
    (define-key map "r" #'kargo-run)
    (define-key map "t" #'kargo-test)
    (define-key map "." #'kargo-test-at-point)
    (define-key map "k" #'kargo-test-class)
    (define-key map "f" #'kargo-test-file)
    (define-key map "F" #'kargo-test-rerun-failed)
    (define-key map "p" #'kargo-test-pick)
    (define-key map "v" #'kargo-test-results)
    (define-key map "C" #'kargo-clean)
    (define-key map "a" #'kargo-add)
    (define-key map "d" #'kargo-remove)
    (define-key map "u" #'kargo-update)
    (define-key map "g" #'kargo-fetch)
    (define-key map "j" #'kargo-task)
    (define-key map "l" #'kargo-runnables)
    (define-key map "T" #'kargo-set-target)
    (define-key map "R" #'kargo-toggle-release)
    (define-key map "x" #'kargo-command)
    (define-key map "e" #'kargo-tree)
    (define-key map "m" #'kargo-menu)
    map)
  "The commands of `kargo-mode', under its prefix key.")

(defcustom kargo-keymap-prefix (kbd "C-c k")
  "The prefix key of the commands in `kargo-mode-map'.
Set it before `kargo' loads, or call `kargo-bind-prefix' after changing it."
  :type 'key-sequence)

(defvar kargo-mode-map (make-sparse-keymap) "The keymap of `kargo-mode'.")

(defun kargo-bind-prefix ()
  "Bind `kargo-command-map' to `kargo-keymap-prefix' in `kargo-mode-map'."
  (define-key kargo-mode-map kargo-keymap-prefix kargo-command-map))

(kargo-bind-prefix)

(defun kargo--mode-line ()
  "The mode line text of `kargo-mode': the active target and profile."
  (let* ((root (kargo-project-root))
         (target (and root (kargo-target root)))
         (release (and root (gethash root kargo--release))))
    (concat " Kargo" (when (or target release)
                       (format "[%s]" (string-join (delq nil (list target (and release "release"))) ","))))))

;;;###autoload
(define-minor-mode kargo-mode
  "Minor mode for the files of a Kargo project.
It binds the Kargo commands under `kargo-keymap-prefix' (C-c k):

\\{kargo-command-map}"
  :lighter (:eval (kargo--mode-line))
  :keymap kargo-mode-map)

(defun kargo--turn-on ()
  "Turn on `kargo-mode' in a Kotlin, Java or `kargo.toml' buffer of a Kargo project."
  (when (and buffer-file-name
             (or (apply #'derived-mode-p kargo-source-modes)
                 (and (equal (file-name-nondirectory buffer-file-name) "kargo.toml")
                      (apply #'derived-mode-p kargo-manifest-modes)))
             (kargo-project-root))
    (kargo-mode 1)))

;;;###autoload
(define-globalized-minor-mode global-kargo-mode kargo-mode kargo--turn-on)

;;;; project.el and Projectile

;;;###autoload
(defun kargo-setup-project ()
  "Make `project.el' treat the directory of a `kargo.toml' as a project root.
This is what Eglot uses to find the root of the language server, so a
workspace member or a folder that isn't under version control works."
  (interactive)
  (require 'project)
  (when (boundp 'project-vc-extra-root-markers)
    (add-to-list 'project-vc-extra-root-markers "kargo.toml")))

(declare-function projectile-register-project-type "projectile")

;;;###autoload
(defun kargo-setup-projectile ()
  "Register Kargo as a Projectile project type."
  (interactive)
  (when (require 'projectile nil t)
    (projectile-register-project-type
     'kargo '("kargo.toml")
     :project-file "kargo.toml"
     :compile "kargo build"
     :test "kargo test"
     :run "kargo run"
     :src-dir "src/"
     :test-dir "tests/"
     :test-suffix "Test")))

(with-eval-after-load 'projectile
  (kargo-setup-projectile))

(autoload 'kargo-test "kargo-test" "Run the tests of the current project." t)
(autoload 'kargo-test-at-point "kargo-test" "Run the test at point." t)
(autoload 'kargo-test-class "kargo-test" "Run the tests of the class at point." t)
(autoload 'kargo-test-file "kargo-test" "Run the tests of the classes in this file." t)
(autoload 'kargo-test-pick "kargo-test" "Pick one of the tests `kargo test --list' finds and run it." t)
(autoload 'kargo-test-rerun-failed "kargo-test" "Run the tests that failed last time." t)
(autoload 'kargo-test-results "kargo-test" "Show the results of the last test run." t)
(autoload 'kargo-menu "kargo-transient" "Show a menu of the Kargo commands." t)

(provide 'kargo)
;;; kargo.el ends here
