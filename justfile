# justfile — optional thin wrapper. Taskfile.yml is canonical (what CI runs).
# See specs/01-development-environment.md §5.3.
build:        ; ./scripts/run_task.sh build
test:         ; ./scripts/run_task.sh test-unit
test-fast:    ; ./scripts/run_task.sh test-unit-fast
lint:         ; ./scripts/run_task.sh lint
lint-fix:     ; ./scripts/run_task.sh lint-fix
lint-saevm:   ; ./scripts/run_task.sh lint-saevm
lint-all:     ; ./scripts/run_task.sh lint-all
bazel-build:  ; ./scripts/run_task.sh bazel-build
bazel-test:   ; ./scripts/run_task.sh bazel-test
