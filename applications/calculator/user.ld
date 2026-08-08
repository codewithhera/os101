/* OS101 userspace image layout.
 *
 * Links the whole ELF image (PHDRs included) at USER_BASE, which lives in
 * level-4 slot 1 — a slot the kernel leaves empty so each process can own a
 * private page-table subtree there. See kernel/src/process.rs USER_BASE;
 * the two must agree.
 */

ENTRY(_start)

PHDRS
{
    text   PT_LOAD FLAGS(5); /* R E */
    rodata PT_LOAD FLAGS(4); /* R   */
    data   PT_LOAD FLAGS(6); /* RW  */
}

SECTIONS
{
    . = 0x8010000000;

    /* Keep PHDRs inside the first mapped segment so the loader never has
     * to peek below USER_BASE. */
    .text : ALIGN(0x1000) {
        KEEP(*(.text._start))
        *(.text .text.*)
    } :text

    .rodata : ALIGN(0x1000) {
        *(.rodata .rodata.*)
        *(.eh_frame_hdr)
        *(.eh_frame)
    } :rodata

    .data : ALIGN(0x1000) {
        *(.data .data.*)
        *(.data.rel.ro .data.rel.ro.*)
        *(.got .got.plt)
    } :data

    .bss : ALIGN(0x1000) {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ : {
        *(.note .note.*)
        *(.comment)
        *(.gnu.build.attributes .gnu.build.attributes.*)
        *(.rela.* .rel.*)
    }
}
