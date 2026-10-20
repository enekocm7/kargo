;;; kargo-tests.el --- Tests of the Kargo package for Emacs  -*- lexical-binding: t; -*-

;; SPDX-License-Identifier: MIT OR Apache-2.0

;;; Commentary:

;; Run from editors/emacs with:
;;
;;   emacs -Q --batch -L . -l test/kargo-tests.el -f ert-run-tests-batch-and-exit
;;
;; They need no Kargo binary: the messages are fixtures, and the tests that run a command use
;; a shell script that prints a fixture.  The tests of the lsp-mode and Dape files run when
;; those packages are installed.

;;; Code:

(require 'ert)
(require 'cl-lib)
(require 'kargo)
(require 'kargo-test)
(require 'kargo-flymake)
(require 'kargo-eglot)

(defun kargo-tests--json-line (&rest pairs)
  "A JSON object line from PAIRS, a plist of strings, numbers, t and :null."
  (concat "{"
          (mapconcat
           (lambda (pair)
             (format "\"%s\":%s" (substring (symbol-name (car pair)) 1)
                     (let ((value (cdr pair)))
                       (cond ((eq value :null) "null")
                             ((eq value t) "true")
                             ((eq value :false) "false")
                             ((numberp value) (number-to-string value))
                             (t (json-encode-string value))))))
           (cl-loop for (key value) on pairs by #'cddr collect (cons key value))
           ",")
          "}"))

(defmacro kargo-tests--with-project (var files &rest body)
  "Run BODY with VAR bound to the directory of a new project that has FILES.
FILES is a list of (RELATIVE-NAME . CONTENTS)."
  (declare (indent 2))
  `(let* ((,var (file-name-as-directory (make-temp-file "kargo-test" t))))
     (unwind-protect
         (progn
           (dolist (file ,files)
             (let ((path (expand-file-name (car file) ,var)))
               (make-directory (file-name-directory path) t)
               (with-temp-file path (insert (cdr file)))))
           (let ((default-directory ,var))
             ,@body))
       (delete-directory ,var t))))

(defconst kargo-tests--error-line
  (kargo-tests--json-line :reason "compiler-message" :severity "error" :message "type mismatch"
                          :file "/p/src/Main.kt" :line 2 :column 16 :target "jvm"
                          :rendered "src/Main.kt:2:16: error: type mismatch\n    val x: Int = \"a\"\n               ^"))

;;;; Messages

(ert-deftest kargo-messages-parse-skips-other-output ()
  (should-not (kargo-messages-parse "   Compiling my-app v0.1.0 (jvm)"))
  (should-not (kargo-messages-parse "{\"no\":\"reason\"}"))
  (should-not (kargo-messages-parse "{broken"))
  (should-not (kargo-messages-parse ""))
  (should (equal "build-finished"
                 (alist-get 'reason (kargo-messages-parse "{\"reason\":\"build-finished\",\"success\":false}")))))

(ert-deftest kargo-messages-decode-keeps-only-messages ()
  (let ((messages (kargo-messages-decode
                   (concat "Compiling x\n" kargo-tests--error-line "\n"
                           "{\"reason\":\"build-finished\",\"success\":false}\n"))))
    (should (equal '("compiler-message" "build-finished")
                   (mapcar (lambda (message) (alist-get 'reason message)) messages)))))

(ert-deftest kargo-messages-json-reads-objects-and-arrays ()
  (let ((value (kargo-messages-json "{\"a\":[1,2],\"b\":{\"c\":null},\"d\":false}")))
    (should (equal '(1 2) (alist-get 'a value)))
    (should-not (alist-get 'd value))
    (should-not (alist-get 'c (alist-get 'b value)))))

(ert-deftest kargo-messages-format-compiler-gives-a-link ()
  (let ((text (kargo-messages-format-compiler (kargo-messages-parse kargo-tests--error-line))))
    (should (string-prefix-p (format "%s:2:16: error: type mismatch" (expand-file-name "/p/src/Main.kt")) text))
    (should (string-match-p "val x: Int" text))))

(ert-deftest kargo-messages-format-compiler-without-column-or-file ()
  (let ((warning (kargo-messages-parse
                  (kargo-tests--json-line :reason "compiler-message" :severity "warning" :message "unused"
                                          :file "/p/A.java" :line 4 :column :null)))
        (linker (kargo-messages-parse
                 (kargo-tests--json-line :reason "compiler-message" :severity "error" :message "linker failed"
                                         :file :null :line :null))))
    (should (equal (format "%s:4: warning: unused" (expand-file-name "/p/A.java"))
                   (kargo-messages-format-compiler warning)))
    (should (equal "error: linker failed" (kargo-messages-format-compiler linker)))))

(ert-deftest kargo-messages-diagnostics-need-a-file-and-a-line ()
  (let ((diagnostics (kargo-messages-diagnostics
                      (kargo-messages-decode
                       (concat kargo-tests--error-line "\n"
                               (kargo-tests--json-line :reason "compiler-message" :severity "error"
                                                       :message "no location" :file :null :line :null))))))
    (should (equal (list (list (expand-file-name "/p/src/Main.kt") 2 16 'error "type mismatch"))
                   diagnostics))))

(ert-deftest kargo-messages-format-test-results ()
  (let ((passed (kargo-messages-test-result
                 (kargo-messages-parse
                  (kargo-tests--json-line :reason "test-finished" :suite "com.example.MathTest" :name "adds"
                                          :outcome "passed" :duration-ms 12))))
        (failed (kargo-messages-test-result
                 (kargo-messages-parse
                  (kargo-tests--json-line :reason "test-finished" :suite "com.example.MathTest$Inner"
                                          :name "divides" :outcome "failed" :message "expected 2"
                                          :details "at com.example.MathTest.divides(MathTest.kt:9)")))))
    (should (equal "test com.example.MathTest.adds ... ok (12 ms)" (kargo-messages-format-test passed)))
    (should (equal "com.example.MathTest.Inner.divides" (kargo-messages-test-title failed)))
    (should (equal (concat "test com.example.MathTest.Inner.divides ... FAILED\n"
                           "    expected 2\n"
                           "    at com.example.MathTest.divides(MathTest.kt:9)")
                   (kargo-messages-format-test failed)))
    (should (equal '(1 1 0) (kargo-messages-counts (list passed failed))))))

(ert-deftest kargo-messages-format-summary-line ()
  (should (equal "test result: 3 passed; 1 failed; 2 ignored (jvm)"
                 (kargo-messages-format-summary
                  (kargo-messages-parse
                   (kargo-tests--json-line :reason "test-summary" :target "jvm" :passed 3 :failed 1
                                           :ignored 2 :success :false))))))

;;;; The compilation error regexp

(defun kargo-tests--match (line)
  "The groups (FILE LINE COLUMN WARNING INFO) the Kargo error regexp finds in LINE, or nil."
  (when (string-match (nth 1 (assq 'kargo compilation-error-regexp-alist-alist)) line)
    (mapcar (lambda (group) (match-string group line)) '(1 2 3 4 5))))

(ert-deftest kargo-error-regexp-matches-compiler-lines ()
  (should (equal '("/p/src/Main.kt" "2" "16" nil nil) (kargo-tests--match "/p/src/Main.kt:2:16: error: x")))
  (should (equal '("src/Main.kt" "7" "1" "warning" nil) (kargo-tests--match "src/Main.kt:7:1: warning: x")))
  (should (equal '("/p/A.java" "4" nil nil nil) (kargo-tests--match "/p/A.java:4: error: x")))
  (should (equal '("C:/p/Main.kt" "3" "1" nil "info") (kargo-tests--match "C:/p/Main.kt:3:1: info: x")))
  (should (equal '("/p/MathTest.kt" "9" nil nil nil) (kargo-tests--match "    /p/MathTest.kt:9: error: at a.B.c(MathTest.kt:9)"))))

(ert-deftest kargo-error-regexp-ignores-other-lines ()
  (should-not (kargo-tests--match "   Compiling my-app v0.1.0 (jvm)"))
  (should-not (kargo-tests--match "test com.example.MathTest.adds ... ok (12 ms)"))
  (should-not (kargo-tests--match "    val x: Int = \"a\"")))

;;;; The filter

(ert-deftest kargo-filter-rewrites-json-lines-and-waits-for-whole-lines ()
  (with-temp-buffer
    (let ((first (substring kargo-tests--error-line 0 40))
          (rest (substring kargo-tests--error-line 40)))
      (setq-local compilation-filter-start (copy-marker (point-min)))
      (insert "   Compiling x\n" first)
      (kargo--filter)
      (should (equal (concat "   Compiling x\n" first) (buffer-string)))
      (set-marker compilation-filter-start (point-max))
      (insert rest "\n{\"reason\":\"build-finished\",\"success\":false}\n")
      (kargo--filter)
      (should (string-prefix-p (concat "   Compiling x\n" (expand-file-name "/p/src/Main.kt") ":2:16: error: type mismatch\n")
                               (buffer-string)))
      (should-not (string-match-p "build-finished" (buffer-string)))
      (should (equal '("build-finished" "compiler-message")
                     (mapcar (lambda (message) (alist-get 'reason message)) kargo--messages))))))

(ert-deftest kargo-stack-frames-link-the-frames-in-the-project ()
  (kargo-tests--with-project root '(("kargo.toml" . "")
                                    ("tests/com/example/MathTest.kt" . "")
                                    ("src/com/example/Math.kt" . ""))
    (setq kargo--source-index nil)
    (let ((frames (kargo--stack-frames
                   root
                   (concat "at com.example.MathTest.divides(MathTest.kt:9)\n"
                           "at org.junit.Foo.bar(Foo.java:1)\n"
                           "  at com.example.Math.div(Math.kt:3)\n"))))
      (should (equal (format "    %s:9: error: at com.example.MathTest.divides(MathTest.kt:9)"
                             (expand-file-name "tests/com/example/MathTest.kt" root))
                     (nth 0 frames)))
      (should (equal "    at org.junit.Foo.bar(Foo.java:1)" (nth 1 frames)))
      (should (equal (format "    %s:3: info: at com.example.Math.div(Math.kt:3)"
                             (expand-file-name "src/com/example/Math.kt" root))
                     (nth 2 frames))))))

(ert-deftest kargo-find-source-prefers-the-package-directory ()
  (kargo-tests--with-project root '(("src/com/a/Util.kt" . "") ("src/com/b/Util.kt" . ""))
    (setq kargo--source-index nil)
    (should (equal (expand-file-name "src/com/b/Util.kt" root) (kargo--find-source root "Util.kt" "com.b.Util")))
    (should-not (kargo--find-source root "Missing.kt" "com.b.Missing"))))

;;;; The project

(ert-deftest kargo-project-root-finds-the-nearest-manifest ()
  (kargo-tests--with-project root '(("kargo.toml" . "") ("member/kargo.toml" . "") ("member/src/a/B.kt" . "")
                                    ("other/C.kt" . ""))
    (should (equal (expand-file-name "member/" root) (kargo-project-root (expand-file-name "member/src/a/" root))))
    (should (equal (file-name-as-directory root) (kargo-project-root (expand-file-name "other/" root))))
    (should-not (kargo-project-root temporary-file-directory))))

(ert-deftest kargo-executable-missing-is-explained ()
  (let ((kargo-executable "kargo-that-does-not-exist-anywhere"))
    (let ((error (should-error (kargo--executable) :type 'user-error)))
      (should (string-match-p "Kargo is not installed" (cadr error)))
      (should (string-match-p "kargo-executable" (cadr error))))))

(ert-deftest kargo-build-args-follow-target-and-profile ()
  (let ((root "/tmp/kargo-args-test/"))
    (unwind-protect
        (progn
          (should-not (kargo--build-args root))
          (puthash root "wasm" kargo--targets)
          (puthash root t kargo--release)
          (should (equal '("-t" "wasm" "--release") (kargo--build-args root))))
      (remhash root kargo--targets)
      (remhash root kargo--release))))

;;;; Tests in a buffer

(defconst kargo-tests--kotlin-source
  "package com.example

import org.junit.jupiter.api.Test

class MathTest {
    @Test
    fun adds() {
    }

    fun helper() {}

    @Test fun `divides by two`() {
    }
}

internal class OtherTest {
    @ParameterizedTest
    @ValueSource(ints = [1, 2])
    fun param(n: Int) {}
}
")

(ert-deftest kargo-test-scan-finds-classes-and-tests ()
  (with-temp-buffer
    (insert kargo-tests--kotlin-source)
    (let* ((scan (kargo-test--scan))
           (tests (plist-get scan :tests)))
      (should (equal "com.example" (plist-get scan :package)))
      (should (equal '("MathTest" "OtherTest") (mapcar #'car (plist-get scan :classes))))
      (should (equal '("com.example.MathTest.adds" "com.example.MathTest.divides by two"
                       "com.example.OtherTest.param")
                     (mapcar #'kargo-test--test-filter tests)))
      (should (equal '(7 12 19) (mapcar (lambda (test) (nth 1 test)) tests))))))

(ert-deftest kargo-test-at-point-picks-the-nearest-test-above ()
  (with-temp-buffer
    (insert kargo-tests--kotlin-source)
    (let ((tests (plist-get (kargo-test--scan) :tests)))
      (should (equal "adds" (car (kargo-test--last-before tests 8))))
      (should (equal "divides by two" (car (kargo-test--last-before tests 14))))
      (should-not (kargo-test--last-before tests 3)))))

(ert-deftest kargo-test-failed-filters-are-unique ()
  (let ((results '(((suite . "a.B") (name . "x") (outcome . "failed"))
                   ((suite . "a.B") (name . "x") (outcome . "failed"))
                   ((suite . "a.B") (name . "y") (outcome . "passed")))))
    (should (equal '("a.B.x") (kargo-test--failed-filters results)))))

;;;; Running commands against a fake kargo

(defun kargo-tests--fake-kargo (directory)
  "Write a `kargo' script into DIRECTORY that prints the file named by $KARGO_FIXTURE."
  (let ((script (expand-file-name "kargo" directory)))
    (with-temp-file script
      (insert "#!/bin/sh\ncat \"$KARGO_FIXTURE\"\nexit \"${KARGO_EXIT:-0}\"\n"))
    (set-file-modes script #o755)
    script))

(defun kargo-tests--run-fake (root fixture exit-code command)
  "Call COMMAND in ROOT with a fake `kargo' that prints FIXTURE and exits with EXIT-CODE.
Return the compilation buffer, after the command has ended."
  (let* ((kargo-executable (kargo-tests--fake-kargo root))
         (process-environment
          (append (list (concat "KARGO_FIXTURE=" (expand-file-name "fixture.jsonl" root))
                        (format "KARGO_EXIT=%d" exit-code))
                  process-environment))
         (done nil)
         (watch (lambda (&rest _) (setq done t)))
         (default-directory root)
         (kargo-test-display-results nil)
         (compilation-ask-about-save nil)
         buffer)
    (with-temp-file (expand-file-name "fixture.jsonl" root) (insert fixture))
    (add-hook 'kargo-finished-functions watch)
    (unwind-protect
        (progn
          (setq buffer (funcall command))
          (let ((deadline (+ (float-time) 20)))
            (while (and (not done) (< (float-time) deadline))
              (accept-process-output nil 0.1)))
          (should done)
          (or buffer (get-buffer (format "*kargo build: %s*" (kargo--project-name root)))))
      (remove-hook 'kargo-finished-functions watch))))

(ert-deftest kargo-build-shows-errors-as-links ()
  (skip-unless (not (eq system-type 'windows-nt)))
  (kargo-tests--with-project root '(("kargo.toml" . ""))
    (let ((buffer (kargo-tests--run-fake root (concat kargo-tests--error-line "\n"
                                                      "{\"reason\":\"build-finished\",\"success\":false}\n")
                                         101 #'kargo-build)))
      (with-current-buffer buffer
        (should (string-match-p (regexp-quote (format "%s:2:16: error: type mismatch" (expand-file-name "/p/src/Main.kt")))
                                (buffer-string)))
        (should-not (string-match-p "build-finished" (buffer-string)))
        (should (equal "build" kargo--subcommand))
        (should (derived-mode-p 'kargo-compilation-mode))))))

(ert-deftest kargo-test-collects-the-results-of-a-run ()
  (skip-unless (not (eq system-type 'windows-nt)))
  (kargo-tests--with-project root '(("kargo.toml" . "")
                                    ("tests/com/example/MathTest.kt" . "package com.example\n"))
    (setq kargo--source-index nil)
    (remhash root kargo-test--state)
    (let ((buffer (kargo-tests--run-fake
                   root
                   (concat
                    (kargo-tests--json-line :reason "test-finished" :suite "com.example.MathTest" :name "adds"
                                            :outcome "passed" :duration-ms 3)
                    "\n"
                    (kargo-tests--json-line :reason "test-finished" :suite "com.example.MathTest" :name "divides"
                                            :outcome "failed" :message "expected 2"
                                            :details "at com.example.MathTest.divides(MathTest.kt:9)")
                    "\n"
                    (kargo-tests--json-line :reason "test-summary" :target "jvm" :passed 1 :failed 1 :ignored 0
                                            :success :false)
                    "\n")
                   101 #'kargo-test)))
      (let ((results (plist-get (gethash root kargo-test--state) :results)))
        (should (equal '("passed" "failed") (mapcar (lambda (result) (alist-get 'outcome result)) results)))
        (should (equal '("com.example.MathTest.divides") (kargo-test--failed-filters results))))
      (with-current-buffer buffer
        (should (string-match-p "test com.example.MathTest.adds ... ok (3 ms)" (buffer-string)))
        (should (string-match-p "test com.example.MathTest.divides ... FAILED" (buffer-string)))
        (should (string-match-p (regexp-quote (format "%s:9: error: at com.example.MathTest.divides"
                                                      (expand-file-name "tests/com/example/MathTest.kt" root)))
                                (buffer-string)))
        (should (string-match-p "test result: 1 passed; 1 failed; 0 ignored (jvm)" (buffer-string)))))))

(ert-deftest kargo-test-results-buffer-lists-each-test ()
  (kargo-tests--with-project root '(("kargo.toml" . ""))
    (let ((results '(((suite . "a.B") (name . "x") (outcome . "passed") (duration . 4))
                     ((suite . "a.B") (name . "y") (outcome . "failed") (message . "no")))))
      (kargo-test--show root results)
      (let ((buffer (get-buffer (format "*kargo tests: %s*" (kargo--project-name root)))))
        (unwind-protect
            (with-current-buffer buffer
              (should (derived-mode-p 'kargo-test-results-mode))
              (should (string-match-p "a.B.x" (buffer-string)))
              (should (string-match-p "FAIL" (buffer-string)))
              (should (string-match-p "4 ms" (buffer-string))))
          (kill-buffer buffer))))))

;;;; Flymake

(ert-deftest kargo-flymake-keeps-diagnostics-by-file ()
  (kargo-tests--with-project root '(("kargo.toml" . ""))
    (let ((file (expand-file-name "src/Main.kt" root)))
      (kargo-flymake--store
       root (kargo-messages-decode
             (concat (kargo-tests--json-line :reason "compiler-message" :severity "error" :message "boom"
                                             :file file :line 3 :column 9 :target "jvm")
                     "\n"
                     (kargo-tests--json-line :reason "compiler-message" :severity "error" :message "boom"
                                             :file file :line 3 :column 9 :target "native")
                     "\n")))
      (let ((table (gethash root kargo-flymake--diagnostics)))
        (should (equal '((3 9 error "boom")) (gethash (file-truename file) table)))))))

;;;; Eglot

(defun kargo-tests--emacs ()
  "An executable that exists everywhere the tests run: this Emacs."
  (expand-file-name invocation-name invocation-directory))

(ert-deftest kargo-eglot-contact-follows-the-options ()
  (let ((kargo-executable (kargo-tests--emacs))
        (kargo-lsp-server nil) (kargo-lsp-server-path nil) (kargo-lsp-klib-targets nil)
        (kargo-lsp-check-on-save 'default) (kargo-lsp-check-targets nil))
    (should (equal '("lsp") (cdr (kargo-eglot-contact))))
    (let ((kargo-lsp-server "kotlin-lsp") (kargo-lsp-check-on-save nil) (kargo-lsp-check-targets '("wasm")))
      (let ((contact (kargo-eglot-contact)))
        (should (equal '("lsp" "--server" "kotlin-lsp") (seq-take (cdr contact) 3)))
        (should (equal [ "wasm" ] (plist-get (plist-get (nthcdr 3 (cdr contact)) :initializationOptions)
                                             :checkTargets)))
        (should (eq :json-false (plist-get (plist-get (nthcdr 3 (cdr contact)) :initializationOptions)
                                           :checkOnSave)))))))

(ert-deftest kargo-eglot-setup-registers-the-server-once ()
  (let ((eglot-server-programs nil)
        (project-vc-extra-root-markers nil)
        (kargo-eglot-auto-start nil))
    (kargo-eglot-setup)
    (kargo-eglot-setup)
    (should (= 1 (length eglot-server-programs)))
    (should (eq #'kargo-eglot-contact (cdar eglot-server-programs)))
    (should (memq 'kotlin-mode (caar eglot-server-programs)))
    (should (equal '("kargo.toml") project-vc-extra-root-markers))))

;;;; lsp-mode and Dape, when installed

(ert-deftest lsp-kargo-activates-in-kargo-projects ()
  (skip-unless (require 'lsp-kargo nil t))
  (kargo-tests--with-project root '(("kargo.toml" . "") ("src/Main.kt" . ""))
    (should (lsp-kargo--activate-p (expand-file-name "src/Main.kt" root) 'kotlin-mode))
    (should (lsp-kargo--activate-p (expand-file-name "kargo.toml" root) 'conf-toml-mode))
    (should-not (lsp-kargo--activate-p (expand-file-name "other.toml" root) 'conf-toml-mode))
    (should-not (lsp-kargo--activate-p (expand-file-name "Main.kt" temporary-file-directory) 'kotlin-mode))))

(ert-deftest kargo-dape-setup-adds-the-configurations ()
  (skip-unless (require 'kargo-dape nil t))
  (let ((dape-configs nil))
    (kargo-dape-setup)
    (should (equal "launch" (plist-get (alist-get 'kargo dape-configs) :request)))
    (should (equal ["test"] (plist-get (alist-get 'kargo-test dape-configs) :args)))
    (should (equal "attach" (plist-get (alist-get 'kargo-attach dape-configs) :request)))))

(provide 'kargo-tests)
;;; kargo-tests.el ends here
