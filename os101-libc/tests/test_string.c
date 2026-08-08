/*
 * string.h and ctype.h against the host's.
 *
 * The interesting cases are the boundaries: an empty string, a length of zero,
 * a needle that runs off the end, memmove's overlap in both directions, and
 * strncpy's rule that it pads with NULs but does not terminate if the source
 * filled the buffer. Sizes and offsets are swept so that the word-at-a-time
 * paths in memcpy, memmove and memset are entered at every alignment.
 */
#include <ctype.h>
#include <stdio.h>
#include <string.h>

#include "harness.h"
#include "os101_api.h"

static void mem_functions(void)
{
    static const size_t SIZES[] = {0, 1, 2, 3, 7, 8, 9, 15, 16, 17, 31,
                                   32, 33, 63, 64, 100, 255, 256, 1000};
    size_t s;
    size_t off;

    for (s = 0; s < sizeof(SIZES) / sizeof(SIZES[0]); s++) {
        size_t n = SIZES[s];
        for (off = 0; off < 17; off++) {
            char mine[1100];
            char theirs[1100];
            char source[1100];
            size_t i;

            for (i = 0; i < sizeof(source); i++) {
                source[i] = (char)(i * 7 + 1);
            }
            memset(mine, '@', sizeof(mine));
            memset(theirs, '@', sizeof(theirs));

            os101_memcpy(mine + off, source, n);
            memcpy(theirs + off, source, n);
            CHECK(memcmp(mine, theirs, sizeof(mine)) == 0,
                  "memcpy(n=%zu, off=%zu)", n, off);

            os101_memset(mine + off, 0x5a, n);
            memset(theirs + off, 0x5a, n);
            CHECK(memcmp(mine, theirs, sizeof(mine)) == 0,
                  "memset(n=%zu, off=%zu)", n, off);

            /* Overlapping upwards and downwards, which is the whole point of
               memmove. */
            memcpy(mine, source, sizeof(mine));
            memcpy(theirs, source, sizeof(theirs));
            os101_memmove(mine + off + 8, mine + off, n);
            memmove(theirs + off + 8, theirs + off, n);
            CHECK(memcmp(mine, theirs, sizeof(mine)) == 0,
                  "memmove up(n=%zu, off=%zu)", n, off);

            memcpy(mine, source, sizeof(mine));
            memcpy(theirs, source, sizeof(theirs));
            os101_memmove(mine + off, mine + off + 8, n);
            memmove(theirs + off, theirs + off + 8, n);
            CHECK(memcmp(mine, theirs, sizeof(mine)) == 0,
                  "memmove down(n=%zu, off=%zu)", n, off);
        }
    }

    {
        const char *a = "abcdefgh";
        const char *b = "abcdefgi";
        size_t n;

        for (n = 0; n <= 8; n++) {
            int mine = os101_memcmp(a, b, n);
            int theirs = memcmp(a, b, n);
            CHECK((mine < 0) == (theirs < 0) && (mine > 0) == (theirs > 0),
                  "memcmp(n=%zu): got %d, host %d", n, mine, theirs);
        }
        /* memcmp is on unsigned bytes: 0x80 is above 0x7f, not below zero. */
        CHECK(os101_memcmp("\x80", "\x7f", 1) > 0, "memcmp is signed");
        CHECK(os101_memchr(a, 'd', 8) == a + 3, "memchr");
        CHECK(os101_memchr(a, 'z', 8) == NULL, "memchr miss");
        CHECK(os101_memchr(a, 'a', 0) == NULL, "memchr with n=0");
    }
}

static void str_functions(void)
{
    static const char *const WORDS[] = {"",      "a",    "ab",   "abc",
                                        "hello", "hello world", "aaa",
                                        "the quick brown fox"};
    size_t i;
    size_t j;

    for (i = 0; i < sizeof(WORDS) / sizeof(WORDS[0]); i++) {
        const char *a = WORDS[i];

        CHECK(os101_strlen(a) == strlen(a), "strlen(\"%s\")", a);
        for (j = 0; j <= strlen(a) + 2; j++) {
            CHECK(os101_strnlen(a, j) == (strlen(a) < j ? strlen(a) : j),
                  "strnlen(\"%s\", %zu)", a, j);
        }

        for (j = 0; j < sizeof(WORDS) / sizeof(WORDS[0]); j++) {
            const char *b = WORDS[j];
            int mine = os101_strcmp(a, b);
            int theirs = strcmp(a, b);
            size_t n;

            CHECK((mine < 0) == (theirs < 0) && (mine > 0) == (theirs > 0),
                  "strcmp(\"%s\", \"%s\"): got %d, host %d", a, b, mine,
                  theirs);
            for (n = 0; n <= 8; n++) {
                mine = os101_strncmp(a, b, n);
                theirs = strncmp(a, b, n);
                CHECK((mine < 0) == (theirs < 0) && (mine > 0) == (theirs > 0),
                      "strncmp(\"%s\", \"%s\", %zu)", a, b, n);
            }
            {
                char *found_mine = os101_strstr(a, b);
                char *found_theirs = strstr(a, b);
                CHECK(found_mine == found_theirs,
                      "strstr(\"%s\", \"%s\") disagreed", a, b);
            }
        }

        {
            char buf_mine[64];
            char buf_theirs[64];
            size_t n;

            memset(buf_mine, '#', sizeof(buf_mine));
            memset(buf_theirs, '#', sizeof(buf_theirs));
            os101_strcpy(buf_mine, a);
            strcpy(buf_theirs, a);
            CHECK(memcmp(buf_mine, buf_theirs, sizeof(buf_mine)) == 0,
                  "strcpy(\"%s\")", a);

            for (n = 0; n <= 12; n++) {
                memset(buf_mine, '#', sizeof(buf_mine));
                memset(buf_theirs, '#', sizeof(buf_theirs));
                os101_strncpy(buf_mine, a, n);
                strncpy(buf_theirs, a, n);
                CHECK(memcmp(buf_mine, buf_theirs, sizeof(buf_mine)) == 0,
                      "strncpy(\"%s\", %zu)", a, n);

                memset(buf_mine, 0, sizeof(buf_mine));
                memset(buf_theirs, 0, sizeof(buf_theirs));
                strcpy(buf_mine, "base:");
                strcpy(buf_theirs, "base:");
                os101_strncat(buf_mine, a, n);
                strncat(buf_theirs, a, n);
                CHECK(memcmp(buf_mine, buf_theirs, sizeof(buf_mine)) == 0,
                      "strncat(\"%s\", %zu)", a, n);
            }

            memset(buf_mine, 0, sizeof(buf_mine));
            memset(buf_theirs, 0, sizeof(buf_theirs));
            strcpy(buf_mine, "base:");
            strcpy(buf_theirs, "base:");
            os101_strcat(buf_mine, a);
            strcat(buf_theirs, a);
            CHECK(strcmp(buf_mine, buf_theirs) == 0, "strcat(\"%s\")", a);
        }

        {
            int c;
            for (c = 'a'; c <= 'e'; c++) {
                CHECK(os101_strchr(a, c) == strchr(a, c), "strchr(\"%s\", %c)",
                      a, c);
                CHECK(os101_strrchr(a, c) == strrchr(a, c),
                      "strrchr(\"%s\", %c)", a, c);
            }
            /* A search for the terminator finds it. */
            CHECK(os101_strchr(a, '\0') == a + strlen(a), "strchr for NUL");
            CHECK(os101_strspn(a, "abcdefgh") == strspn(a, "abcdefgh"),
                  "strspn(\"%s\")", a);
            CHECK(os101_strcspn(a, "xyz ") == strcspn(a, "xyz "),
                  "strcspn(\"%s\")", a);
            CHECK(os101_strpbrk(a, "wxo") == strpbrk(a, "wxo"),
                  "strpbrk(\"%s\")", a);
        }
    }
}

static void tokenising(void)
{
    char mine[64];
    char theirs[64];
    char *pm;
    char *pt;
    int n = 0;

    strcpy(mine, "  one,two;;three  ");
    strcpy(theirs, "  one,two;;three  ");
    pm = os101_strtok(mine, " ,;");
    pt = strtok(theirs, " ,;");
    while (pm != NULL && pt != NULL) {
        CHECK(strcmp(pm, pt) == 0, "strtok token %d: \"%s\" vs \"%s\"", n, pm,
              pt);
        pm = os101_strtok(NULL, " ,;");
        pt = strtok(NULL, " ,;");
        n++;
    }
    CHECK(pm == NULL && pt == NULL, "strtok did not end together");
    CHECK(n == 3, "strtok found %d tokens, expected 3", n);
}

static void duplication(void)
{
    char *copy = os101_strdup("a string to duplicate");

    CHECK(copy != NULL, "strdup returned NULL");
    if (copy != NULL) {
        CHECK(strcmp(copy, "a string to duplicate") == 0, "strdup contents");
        os101_free(copy);
    }
    copy = os101_strdup("");
    CHECK(copy != NULL && copy[0] == '\0', "strdup of an empty string");
    os101_free(copy);
}

static void error_strings(void)
{
    /* Not compared against the host's wording, only checked for being a
       non-empty string and distinct for a few values. */
    CHECK(os101_strlen(os101_strerror(0)) > 0, "strerror(0) is empty");
    CHECK(os101_strlen(os101_strerror(12)) > 0, "strerror(ENOMEM) is empty");
    CHECK(strcmp(os101_strerror(38), os101_strerror(22)) != 0,
          "ENOSYS and EINVAL share a message");
    CHECK(os101_strlen(os101_strerror(9999)) > 0, "strerror of a bad value");
}

static void ctype_functions(void)
{
    int c;

    for (c = -1; c < 256; c++) {
        CHECK(!os101_isdigit(c) == !isdigit(c), "isdigit(%d)", c);
        CHECK(!os101_isalpha(c) == !isalpha(c), "isalpha(%d)", c);
        CHECK(!os101_isalnum(c) == !isalnum(c), "isalnum(%d)", c);
        CHECK(!os101_isspace(c) == !isspace(c), "isspace(%d)", c);
        CHECK(!os101_isupper(c) == !isupper(c), "isupper(%d)", c);
        CHECK(!os101_islower(c) == !islower(c), "islower(%d)", c);
        CHECK(!os101_isxdigit(c) == !isxdigit(c), "isxdigit(%d)", c);
        CHECK(!os101_isblank(c) == !isblank(c), "isblank(%d)", c);
        if (c >= 0 && c < 128) {
            /* Above 127 the host's tables follow its locale and this library's
               do not; the C locale is only defined up to 127. */
            CHECK(!os101_iscntrl(c) == !iscntrl(c), "iscntrl(%d)", c);
            CHECK(!os101_isprint(c) == !isprint(c), "isprint(%d)", c);
            CHECK(!os101_isgraph(c) == !isgraph(c), "isgraph(%d)", c);
            CHECK(!os101_ispunct(c) == !ispunct(c), "ispunct(%d)", c);
            CHECK(os101_tolower(c) == tolower(c), "tolower(%d)", c);
            CHECK(os101_toupper(c) == toupper(c), "toupper(%d)", c);
        }
    }
}

void run_string_tests(void)
{
    test_section("string.h and ctype.h, against the host's");
    mem_functions();
    str_functions();
    tokenising();
    duplication();
    error_strings();
    ctype_functions();
}
