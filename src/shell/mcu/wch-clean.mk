# Supplement only a managed MounRiver CDT clean target. The stock recipe
# ignores rm errors and omits the post-build BIN and linker MAP.
_qccom_image := $(strip $(filter %.hex %.bin,$(SECONDARY_FLASH)))
ifeq ($(words $(SECONDARY_FLASH)),1)
ifeq ($(dir $(_qccom_image)),./)
_qccom_stem := $(basename $(_qccom_image))
_qccom_extra := "$(_qccom_stem).elf" "$(_qccom_stem).bin" "$(_qccom_stem).map"
endif
endif

.PHONY: qccom-clean-generated
clean: qccom-clean-generated
qccom-clean-generated:
	@$(RM) $(OBJS) $(ASM_DEPS) $(S_UPPER_DEPS) $(C_DEPS) $(CPP_DEPS) $(CXX_DEPS) $(SECONDARY_FLASH) $(SECONDARY_LIST) $(SECONDARY_SIZE) $(_qccom_extra)
