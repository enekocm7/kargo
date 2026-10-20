;;; kargo-transient.el --- A Transient menu for Kargo  -*- lexical-binding: t; -*-

;; SPDX-License-Identifier: MIT OR Apache-2.0

;; Author: The Kargo authors
;; URL: https://github.com/enekocm7/kargo
;; Package-Requires: ((emacs "29.1") (transient "0.4"))

;;; Commentary:

;; `kargo-menu' shows every Kargo command in a Transient menu.  `kargo-mode' binds it to
;; C-c k m.

;;; Code:

(require 'transient)
(require 'kargo)

(defun kargo-transient--target ()
  "The description of the target command: the active target."
  (format "Target (%s)" (or (and (kargo-project-root) (kargo-target)) "all")))

(defun kargo-transient--release ()
  "The description of the profile command: the active profile."
  (format "Profile (%s)" (if (and (kargo-project-root) (gethash (kargo-project-root) kargo--release)) "release" "dev")))

;;;###autoload (autoload 'kargo-menu "kargo-transient" nil t)
(transient-define-prefix kargo-menu ()
  "Run a Kargo command."
  [["Build"
    ("b" "build" kargo-build)
    ("c" "check" kargo-check)
    ("r" "run" kargo-run)
    ("C" "clean" kargo-clean)
    ("l" "runnables..." kargo-runnables)]
   ["Test"
    ("t" "all" kargo-test)
    ("." "at point" kargo-test-at-point)
    ("k" "class" kargo-test-class)
    ("f" "file" kargo-test-file)
    ("p" "pick..." kargo-test-pick)
    ("F" "failed again" kargo-test-rerun-failed)
    ("v" "results" kargo-test-results)]
   ["Dependencies"
    ("a" "add" kargo-add)
    ("d" "remove" kargo-remove)
    ("u" "update" kargo-update)
    ("g" "fetch" kargo-fetch)
    ("e" "tree" kargo-tree)]
   ["Project"
    ("j" "task" kargo-task)
    ("T" kargo-transient--target kargo-set-target)
    ("R" kargo-transient--release kargo-toggle-release)
    ("A" "add target" kargo-target-add)
    ("x" "other command..." kargo-command)]
   ["Debug"
    :if (lambda () (featurep 'kargo-dape))
    ("D" "run" kargo-dape-run)
    ("M" "test at point" kargo-dape-test-at-point)
    ("E" "attach" kargo-dape-attach)]])

(provide 'kargo-transient)
;;; kargo-transient.el ends here
