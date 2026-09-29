;; The readable source of `probe.wasm`, committed beside it.
;;
;; `probe.wasm` is what the browser actually fetches and runs. A binary blob in
;; a diff is not reviewable, so this file is the reviewable form -- and
;; `the_committed_module_matches_its_readable_source` in xtask/src/browser.rs
;; instantiates both with the same import and fails the build if they ever
;; disagree, so the two cannot drift apart.
;;
;; The import is the point. A module with an unsatisfied import must fail to
;; *link* -- before a single instruction runs -- and the page checks that the
;; engine says so. A page that could satisfy its own imports trivially would
;; not be testing anything.
(module
  (import "env" "twice" (func $twice (param i32) (result i32)))
  (func (export "selis_probe_add") (param i32 i32) (result i32)
    local.get 0
    call $twice
    local.get 1
    call $twice
    i32.add))
