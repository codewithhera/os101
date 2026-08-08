/*
 * Hello C — the first OS101 application written in C.
 *
 * It is deliberately not a hello world. Everything here is a part of
 * os101-libc that could plausibly be broken and that the host tests cannot
 * reach, exercised in the order it would break: printf's conversions, then a
 * few thousand allocations with a checksum over them, then qsort, then the
 * maths, and finally a window with a button, because a window is the part that
 * needs the syscall argument packing to be exactly right.
 *
 * Build it with:
 *
 *     ./applications/hello-c/build.sh
 *
 * The console output goes to the framebuffer console and the serial port; the
 * window is what you see on the desktop.
 */
#include <math.h>
#include <os101.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define BLOCK_COUNT 3000

/* Fill a block with a pattern that depends on its own index, so that a wrong
   size or a bad merge shows up as a checksum that does not match. */
static unsigned long fill_block(unsigned char *block, size_t size, int index)
{
    unsigned long sum = 0;
    size_t i;

    for (i = 0; i < size; i++) {
        block[i] = (unsigned char)(index * 7 + (int)i);
        sum += block[i];
    }
    return sum;
}

static unsigned long check_block(const unsigned char *block, size_t size,
                                 int index)
{
    unsigned long sum = 0;
    size_t i;

    for (i = 0; i < size; i++) {
        if (block[i] != (unsigned char)(index * 7 + (int)i)) {
            return 0;
        }
        sum += block[i];
    }
    return sum;
}

static int exercise_malloc(void)
{
    static unsigned char *blocks[BLOCK_COUNT];
    static size_t sizes[BLOCK_COUNT];
    unsigned long written = 0;
    unsigned long read_back = 0;
    int i;
    int failures = 0;

    for (i = 0; i < BLOCK_COUNT; i++) {
        sizes[i] = (size_t)(16 + (i * 37) % 900);
        blocks[i] = malloc(sizes[i]);
        if (blocks[i] == NULL) {
            printf("  malloc failed at block %d\n", i);
            failures++;
            break;
        }
        if (((unsigned long)(size_t)blocks[i] & 15u) != 0) {
            printf("  block %d is not 16-byte aligned\n", i);
            failures++;
        }
        written += fill_block(blocks[i], sizes[i], i);
    }

    /* Free every other block, then allocate again into the holes: the second
       round has to reuse the freed space rather than grow the heap. */
    for (i = 0; i < BLOCK_COUNT; i += 2) {
        free(blocks[i]);
        blocks[i] = NULL;
    }
    for (i = 0; i < BLOCK_COUNT; i += 2) {
        blocks[i] = malloc(sizes[i]);
        if (blocks[i] == NULL) {
            failures++;
            continue;
        }
        fill_block(blocks[i], sizes[i], i);
    }

    for (i = 0; i < BLOCK_COUNT; i++) {
        if (blocks[i] == NULL) {
            continue;
        }
        read_back += check_block(blocks[i], sizes[i], i);
        free(blocks[i]);
    }

    printf("  %d blocks: checksum %lu written, %lu read back — %s\n",
           BLOCK_COUNT, written, read_back,
           written == read_back && failures == 0 ? "ok" : "MISMATCH");
    return failures == 0 && written == read_back;
}

static int compare_int(const void *a, const void *b)
{
    int x = *(const int *)a;
    int y = *(const int *)b;

    return x < y ? -1 : (x > y ? 1 : 0);
}

static int exercise_qsort(void)
{
    int values[400];
    int i;
    int sorted = 1;

    srand(20240501);
    for (i = 0; i < 400; i++) {
        values[i] = rand() % 10000;
    }
    qsort(values, 400, sizeof(values[0]), compare_int);
    for (i = 1; i < 400; i++) {
        if (values[i - 1] > values[i]) {
            sorted = 0;
        }
    }
    printf("  qsort of 400: %d .. %d, %s\n", values[0], values[399],
           sorted ? "in order" : "OUT OF ORDER");

    {
        int key = values[137];
        int *found = bsearch(&key, values, 400, sizeof(values[0]), compare_int);
        printf("  bsearch for %d: %s\n", key,
               found != NULL && *found == key ? "found" : "NOT FOUND");
        if (found == NULL) {
            sorted = 0;
        }
    }
    return sorted;
}

static void exercise_printf(void)
{
    char buf[64];
    int n;

    printf("  %%d %d  %%i %i  %%u %u  %%x %x  %%X %#X  %%o %#o\n", -42, 42,
           4294967295u, 48879u, 48879u, 511u);
    printf("  %%c '%c'  %%s \"%s\"  %%p %p  %%%% %%\n", 'A', "string",
           (void *)exercise_printf);
    printf("  %%f %f  %%.3f %.3f  %%e %e  %%g %g  %%g %g\n", 3.14159265358979,
           2.0 / 3.0, 6.02214076e23, 0.000123456, 123456789.0);
    printf("  padding [%8.3f] [%-8.3f] [%+08.2f] [%8s] [%-8s]\n", 1.5, 1.5,
           -1.5, "right", "left");
    printf("  widths  [%*d] [%.*f]\n", 6, 42, 4, 1.0 / 7.0);

    n = snprintf(buf, 12, "truncate %d %s", 12345, "this");
    printf("  snprintf into 12 bytes: \"%s\", would have written %d\n", buf, n);
}

static void exercise_math(void)
{
    printf("  sqrt(2) = %.15f\n", sqrt(2.0));
    printf("  pow(2, 10) = %.1f   pow(1.5, 3.5) = %.10f\n", pow(2.0, 10.0),
           pow(1.5, 3.5));
    printf("  exp(1) = %.15f   log(exp(1)) = %.15f\n", exp(1.0), log(exp(1.0)));
    printf("  sin(pi/6) = %.15f   cos(pi/3) = %.15f\n", sin(M_PI / 6.0),
           cos(M_PI / 3.0));
    printf("  atan2(1, 1) * 4 = %.15f   (pi = %.15f)\n", atan2(1.0, 1.0) * 4.0,
           M_PI);
    printf("  floor(-2.5) = %.1f  ceil(-2.5) = %.1f  round(-2.5) = %.1f\n",
           floor(-2.5), ceil(-2.5), round(-2.5));
    printf("  fmod(10, 3) = %.1f   hypot(3, 4) = %.1f\n", fmod(10.0, 3.0),
           hypot(3.0, 4.0));
    printf("  strtod(\"0.1\") back out as %%.17g = %.17g\n",
           strtod("0.1", NULL));
}

/* Button action identifiers. Any number will do: the kernel hands back
   whatever was registered. */
#define ACTION_COUNT 1
#define ACTION_RESET 2

int main(void)
{
    uint64_t window;
    uint64_t label;
    uint64_t counter_label;
    int clicks = 0;
    int all_well = 1;

    printf("hello-c: a C application on OS101\n");
    printf("printf:\n");
    exercise_printf();
    printf("malloc:\n");
    all_well = exercise_malloc() && all_well;
    printf("sorting:\n");
    all_well = exercise_qsort() && all_well;
    printf("math:\n");
    exercise_math();
    printf("hello-c: self-test %s\n", all_well ? "passed" : "FAILED");

    window = os101_window_create("Hello C", 320, 170);
    if (window == OS101_SYS_ERROR) {
        printf("hello-c: the kernel refused a window\n");
        return 1;
    }

    os101_label_add(window, 16, 16, "A C program, built with tools/os101-cc.");
    label = os101_label_add(window, 16, 40,
                            all_well ? "libc self-test: passed"
                                     : "libc self-test: FAILED");
    counter_label = os101_label_add(window, 16, 64, "Clicks: 0");
    os101_button_add(window, 16, 96, 130, 30, "Count", ACTION_COUNT);
    os101_button_add(window, 158, 96, 130, 30, "Reset", ACTION_RESET);
    os101_footer_set(window, "printf, malloc, qsort and math all ran");

    for (;;) {
        os101_event ev = os101_event_poll(window);

        if (ev.kind == OS101_EVENT_CLOSED) {
            /* The window has gone: so should the process. */
            printf("hello-c: window closed after %d clicks\n", clicks);
            return 0;
        }
        if (ev.kind == OS101_EVENT_BUTTON) {
            char text[64];

            if (ev.action_id == ACTION_RESET) {
                clicks = 0;
            } else {
                clicks++;
            }
            snprintf(text, sizeof(text), "Clicks: %d", clicks);
            os101_widget_update(window, counter_label, text);
            snprintf(text, sizeof(text), "%d clicks, sqrt(%d) = %.6f", clicks,
                     clicks, sqrt((double)clicks));
            os101_widget_update(window, label, text);
            continue; /* there may be another event waiting */
        }

        /* Nothing to do: give the CPU back. The scheduler is cooperative
           (kernel/src/process.rs), so a poll loop that does not yield stops
           every other process on the machine. */
        os101_yield();
    }
}
