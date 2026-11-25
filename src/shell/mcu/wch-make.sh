#!/bin/sh
if [ "$(pwd -P)" != "$QCCOM_MCU_PROJECT" ] || [ -f GNUmakefile ] || [ -f makefile ] || [ -f Makefile ]; then
    exec "$QCCOM_REAL_MAKE" "$@"
fi
build_only=1
for arg do
    case "$arg" in
        -C*|-f*|--directory*|--file*|--makefile*) exec "$QCCOM_REAL_MAKE" "$@" ;;
        -j*|[0-9]*|--jobs*) ;;
        *) build_only=0 ;;
    esac
done
if [ -n "$QCCOM_MCU_MAKE_DIR" ] &&
    { [ -f "$QCCOM_MCU_MAKE_DIR/GNUmakefile" ] || [ -f "$QCCOM_MCU_MAKE_DIR/makefile" ] || [ -f "$QCCOM_MCU_MAKE_DIR/Makefile" ]; }; then
    if [ "$#" = 1 ] && [ "$1" = clean ] && [ -n "$QCCOM_MRS_CLEAN_RULES" ]; then
        for makefile in GNUmakefile makefile Makefile; do
            path="$QCCOM_MCU_MAKE_DIR/$makefile"
            [ -f "$path" ] || continue
            if head -n 5 "$path" | grep -Fq 'Automatically-generated file. Do not edit!'; then
                set -- -f "$makefile" -f "$QCCOM_MRS_CLEAN_RULES" "$@"
            fi
            break
        done
    fi
    if [ "$build_only" = 1 ]; then set -- "$@" all; fi
    exec "$QCCOM_REAL_MAKE" -C "$QCCOM_MCU_MAKE_DIR" "$@"
fi
if [ -n "$QCCOM_MRS_IDE" ]; then
    for arg do
        case "$arg" in
            clean) exit 0 ;;
            -j*|[0-9]*|--jobs*|all) ;;
            *) exec "$QCCOM_REAL_MAKE" "$@" ;;
        esac
    done
    # The CDT builder invokes make itself. Exclude this shim from that child PATH.
    PATH="$QCCOM_MCU_TOOL_DIRS"; export PATH
    exec "$QCCOM_MRS_IDE" -nosplash --launcher.suppressErrors \
        -application org.eclipse.cdt.managedbuilder.core.headlessbuild \
        -data "$QCCOM_MRS_WORKSPACE" -import "$QCCOM_MCU_PROJECT" -build "$QCCOM_MRS_SELECTION"
fi
exec "$QCCOM_REAL_MAKE" "$@"
