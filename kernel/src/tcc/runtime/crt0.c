/* Minimal CRT for programs compiled inside OS101 by TinyCC. */
extern int main(int argc, char **argv);
extern void __libc_init(void);
void exit(int code);

void _start(void)
{
    int code;
    __libc_init();
    code = main(0, 0);
    exit(code);
}

void __libc_init(void)
{
}
