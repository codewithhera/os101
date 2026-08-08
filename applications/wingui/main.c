#include <stdio.h>
#include <os101.h>

int main(void) {
    printf("wingui: before window\n");
    uint64_t w = os101_window_create("WinGUI", 300, 140);
    printf("wingui: window=%llu\n", (unsigned long long)w);
    if (w == OS101_SYS_ERROR) {
        printf("wingui: create failed\n");
        return 1;
    }
    uint64_t lab = os101_label_add(w, 16, 20, "GUI from C works");
    os101_button_add(w, 16, 60, 120, 30, "OK", 1);
    os101_footer_set(w, "wingui");
    printf("wingui: in event loop\n");
    for (;;) {
        os101_event e = os101_event_poll(w);
        if (e.kind == OS101_EVENT_CLOSED) {
            printf("wingui: closed\n");
            return 0;
        }
        if (e.kind == OS101_EVENT_BUTTON) {
            os101_widget_update(w, lab, "clicked!");
            continue;
        }
        os101_yield();
    }
}
