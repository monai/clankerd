# Apple's Make 3.81 can miss subsecond changes. Force a check, then use the
# system find's nanosecond comparison before running a file-producing recipe.
.PHONY: check-file-times
check-file-times:

define skip_unchanged
if [ -e "$@" ]; then \
    newer=$$(find $(foreach prerequisite,$(filter-out check-file-times,$^),"$(prerequisite)") -prune -newer "$@" -print) || exit 1; \
    [ -n "$$newer" ] || exit 0; \
fi
endef
