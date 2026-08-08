/* OS101 C/C++ application image layout.
 *
 * Based on applications/hello-elf/user.ld, with the two things a C or C++
 * program needs that a Rust one does not: the init_array/fini_array tables
 * that hold static constructors and destructors, and symbols around them so
 * __libc_init can walk the tables before main runs.
 *
 * Every section is page aligned. The kernel's loader (kernel/src/process.rs)
 * maps the pages each PT_LOAD segment touches and then zeroes p_memsz before
 * copying p_filesz, so two segments sharing a page would mean mapping the
 * same page twice; giving each its own pages avoids the question entirely and
 * costs a few kilobytes of image.
 *
 * The address constants come from kernel/src/process.rs:
 *   USER_BASE       0x8010000000  image loads here
 *   USER_HEAP_BASE  0x8020000000  sbrk hands out from here
 * so the image has a 256 MiB window and the ASSERT below is what tells you,
 * at link time, that you have overrun it.
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
    __os101_image_start = .;

    /* The .l* patterns are the large code model's sections, and they are not
     * optional: tools/os101-cc builds with -mcmodel=large (the image is above
     * the 4 GiB that the default model can address), and that puts every
     * function in .ltext.<name> and every string in .lrodata. Left unmatched
     * they become orphans, and lld's orphan placement put all of the code in
     * the read-only segment — which the kernel then maps without PF_X, so the
     * program faulted on its first instruction. Naming them here is what keeps
     * code in the executable segment whichever code model an application is
     * built with. */
    .text : ALIGN(0x1000) {
        KEEP(*(.text._start))
        *(.text .text.*)
        *(.ltext .ltext.*)
        *(.gnu.linkonce.t.*)
    } :text

    .rodata : ALIGN(0x1000) {
        *(.rodata .rodata.*)
        *(.lrodata .lrodata.*)
        *(.gnu.linkonce.r.*)
        *(.eh_frame_hdr)
        *(.eh_frame)
        *(.gcc_except_table .gcc_except_table.*)
    } :rodata

    /* Static constructors. KEEP because nothing references these tables —
     * __libc_init finds them through the symbols below — and --gc-sections
     * would otherwise throw away every global object's constructor. */
    .init_array : ALIGN(0x1000) {
        __init_array_start = .;
        KEEP(*(SORT_BY_INIT_PRIORITY(.init_array.*)))
        KEEP(*(SORT_BY_INIT_PRIORITY(.ctors.*)))
        KEEP(*(.init_array))
        KEEP(*(.ctors))
        __init_array_end = .;
    } :data

    .fini_array : ALIGN(0x1000) {
        __fini_array_start = .;
        KEEP(*(SORT_BY_INIT_PRIORITY(.fini_array.*)))
        KEEP(*(SORT_BY_INIT_PRIORITY(.dtors.*)))
        KEEP(*(.fini_array))
        KEEP(*(.dtors))
        __fini_array_end = .;
    } :data

    .data : ALIGN(0x1000) {
        *(.data .data.*)
        *(.ldata .ldata.*)
        *(.gnu.linkonce.d.*)
        *(.data.rel.ro .data.rel.ro.*)
        *(.got .got.plt)
    } :data

    /* Zeroed by the loader: it clears p_memsz for the segment, and this
     * section is what makes p_memsz larger than p_filesz. */
    .bss : ALIGN(0x1000) {
        __bss_start = .;
        *(.bss .bss.*)
        *(.lbss .lbss.*)
        *(COMMON)
        __bss_end = .;
    } :data

    __os101_image_end = .;

    /DISCARD/ : {
        *(.note .note.*)
        *(.comment)
        *(.gnu.build.attributes .gnu.build.attributes.*)
        *(.rela.* .rel.*)
        *(.interp)
        *(.dynamic)
        *(.hash .gnu.hash)
    }

    ASSERT(__os101_image_start >= 0x8010000000,
           "image starts below the kernel's USER_BASE")
    ASSERT(__os101_image_end <= 0x8020000000,
           "image overruns the 256 MiB window before the heap")
}
