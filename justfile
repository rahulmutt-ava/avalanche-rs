# justfile — optional thin wrapper. Taskfile.yml is canonical (what CI runs).
# See specs/01-development-environment.md §5.3. Full surface owned by
# plan/X-cross-cutting.md.
build:        ; ./scripts/run_task.sh build
test:         ; ./scripts/run_task.sh test-unit
test-fast:    ; ./scripts/run_task.sh test-unit-fast
lint:         ; ./scripts/run_task.sh lint
lint-fix:     ; ./scripts/run_task.sh lint-fix
