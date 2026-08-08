/*
 * The one errno.
 *
 * In .bss, so the ELF loader zeroes it: kernel/src/process.rs clears each
 * segment's p_memsz before copying, which is what lets this file be one line
 * with no initialiser.
 */
int errno;
