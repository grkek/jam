
// Where a core started through PSCI begins. For now it only parks.
	.global	secondary_entry
secondary_entry:
1:	wfe
	b	1b
