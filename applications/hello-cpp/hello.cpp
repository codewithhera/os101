// Hello C++ — the first OS101 application written in C++.
//
// Every part of the C++ runtime that os101-libc/src/cxxrt.cpp provides is
// exercised here, because each one is a symbol the compiler emits a reference
// to and none of them can be tested without running a real program:
//
//   a global object with a constructor    .init_array, walked by __libc_init
//   a destructor at namespace scope       __cxa_atexit, run by exit
//   a function-local static               __cxa_guard_acquire / _release
//   virtual functions through a base      the vtable, and __cxa_pure_virtual
//                                         if one were ever missing
//   new, new[], delete, delete[]          the allocation operators
//   an object with a destructor in a block RAII, which is the whole reason to
//                                         write C++ rather than C
//   a template                            instantiated for three types
//
// This is freestanding C++: classes, templates, RAII, virtual functions, new
// and delete. There is no std::vector and no std::string — see
// os101-libc/README.md — so the little container below is hand-written, which
// is also the point: it is what proves new[] and delete[] work.
//
// Build it with:
//
//     ./applications/hello-cpp/build.sh
//
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <new>
#include <os101.h>

namespace {

// A global object. Its constructor runs before main, from .init_array, and its
// destructor runs from exit through __cxa_atexit.
class Banner {
public:
    Banner()
    {
        printf("hello-cpp: a global object's constructor ran before main\n");
        constructed_ = true;
    }

    ~Banner() { printf("hello-cpp: the global object's destructor ran\n"); }

    bool constructed() const { return constructed_; }

private:
    bool constructed_ = false;
};

Banner banner;

// Virtual functions, called through a base pointer so the call has to go
// through the vtable.
class Shape {
public:
    virtual ~Shape() = default;
    virtual const char *name() const = 0;
    virtual double area() const = 0;

    void describe() const
    {
        printf("  %-9s area %10.4f\n", name(), area());
    }
};

class Circle : public Shape {
public:
    explicit Circle(double radius) : radius_(radius) {}
    const char *name() const override { return "circle"; }
    double area() const override { return M_PI * radius_ * radius_; }

private:
    double radius_;
};

class Rectangle : public Shape {
public:
    Rectangle(double width, double height) : width_(width), height_(height) {}
    const char *name() const override { return "rectangle"; }
    double area() const override { return width_ * height_; }

private:
    double width_;
    double height_;
};

class Triangle : public Rectangle {
public:
    Triangle(double base, double height) : Rectangle(base, height) {}
    const char *name() const override { return "triangle"; }
    double area() const override { return Rectangle::area() * 0.5; }
};

// RAII: the destructor runs when the block ends, however it ends.
class Trace {
public:
    explicit Trace(const char *what) : what_(what)
    {
        printf("  enter %s\n", what_);
    }
    ~Trace() { printf("  leave %s\n", what_); }

private:
    const char *what_;
};

// A template, instantiated below for three different types.
template <typename T> T largest(const T *values, int count)
{
    T best = values[0];
    for (int i = 1; i < count; i++) {
        if (best < values[i]) {
            best = values[i];
        }
    }
    return best;
}

// A hand-written container, so that new[] and delete[] are both used on a type
// with a constructor and a destructor.
template <typename T> class Array {
public:
    explicit Array(int count) : count_(count), items_(new T[count]) {}
    ~Array() { delete[] items_; }

    Array(const Array &) = delete;
    Array &operator=(const Array &) = delete;

    T &operator[](int i) { return items_[i]; }
    const T &operator[](int i) const { return items_[i]; }
    int size() const { return count_; }

private:
    int count_;
    T *items_;
};

// An array of objects that have a destructor is its own case: the compiler
// asks operator new[] for extra room, stores the element count in front of the
// first object, and hands operator delete[] the pointer it originally got — so
// getting this wrong means either a leak or a free of the wrong address.
class Counted {
public:
    Counted() : value_(static_cast<double>(++live_)) {}
    ~Counted() { live_--; }

    double value() const { return value_; }
    static int live() { return live_; }

private:
    static int live_;
    double value_;
};

int Counted::live_ = 0;

// A function-local static with a non-trivial constructor. This is what needs
// the guard variables: the compiler emits a call to __cxa_guard_acquire before
// the constructor and __cxa_guard_release after it, so that the table is built
// exactly once, on the first call.
class Squares {
public:
    Squares()
    {
        printf("  the lookup table was built on first use\n");
        for (int i = 0; i < 16; i++) {
            values_[i] = i * i;
        }
    }

    int at(int i) const { return values_[i]; }

private:
    int values_[16];
};

const Squares &squares()
{
    static Squares table;
    return table;
}

bool exercise_virtuals()
{
    printf("virtual functions through a base pointer:\n");

    Shape *shapes[3];
    shapes[0] = new Circle(2.0);
    shapes[1] = new Rectangle(3.0, 4.0);
    shapes[2] = new Triangle(3.0, 4.0);

    double total = 0.0;
    for (Shape *shape : shapes) {
        shape->describe();
        total += shape->area();
    }
    printf("  total %.4f\n", total);

    for (Shape *shape : shapes) {
        delete shape; // through the base pointer, so the virtual destructor
    }

    const double expected = M_PI * 4.0 + 12.0 + 6.0;
    return fabs(total - expected) < 1e-9;
}

bool exercise_templates_and_new()
{
    printf("templates, new and delete:\n");

    const int ints[] = {3, 17, 5, 11};
    const double doubles[] = {0.5, -2.25, 1.75};
    const char chars[] = {'a', 'q', 'f'};
    printf("  largest int %d, largest double %.2f, largest char %c\n",
           largest(ints, 4), largest(doubles, 3), largest(chars, 3));

    Array<double> values(64);
    for (int i = 0; i < values.size(); i++) {
        values[i] = std::sqrt(static_cast<double>(i));
    }
    printf("  sqrt table: [8] = %.6f, [63] = %.6f\n", values[8], values[63]);

    int *nothrow_block = new (std::nothrow) int[32];
    bool nothrow_ok = nothrow_block != nullptr;
    if (nothrow_ok) {
        for (int i = 0; i < 32; i++) {
            nothrow_block[i] = i * 3;
        }
        nothrow_ok = nothrow_block[31] == 93;
        delete[] nothrow_block;
    }
    printf("  nothrow new[]: %s\n", nothrow_ok ? "ok" : "FAILED");

    // Enough allocation and release to make the allocator split and coalesce.
    bool churn_ok = true;
    for (int round = 0; round < 200; round++) {
        Array<int> *scratch = new Array<int>(1 + round % 97);
        for (int i = 0; i < scratch->size(); i++) {
            (*scratch)[i] = i;
        }
        if ((*scratch)[scratch->size() - 1] != scratch->size() - 1) {
            churn_ok = false;
        }
        delete scratch;
    }
    printf("  200 rounds of new/delete: %s\n", churn_ok ? "ok" : "FAILED");

    Counted *objects = new Counted[8];
    bool counted_ok = Counted::live() == 8 && objects[7].value() == 8.0;
    delete[] objects;
    counted_ok = counted_ok && Counted::live() == 0;
    printf("  new[]/delete[] of 8 objects with destructors: %s\n",
           counted_ok ? "all 8 constructed and destroyed" : "FAILED");

    return nothrow_ok && churn_ok && counted_ok
           && std::fabs(values[63] - std::sqrt(63.0)) < 1e-15;
}

bool exercise_statics()
{
    printf("RAII and a function-local static:\n");
    {
        Trace trace("a scope with an object in it");
        printf("  squares()(12) = %d\n", squares().at(12));
    }
    // The second call must not build the table again.
    printf("  squares()(15) = %d, on the second call\n", squares().at(15));
    return squares().at(12) == 144 && squares().at(15) == 225;
}

void say_goodbye()
{
    printf("hello-cpp: an atexit handler ran\n");
}

constexpr uint64_t kActionNext = 1;
constexpr uint64_t kActionQuit = 2;

} // namespace

int main()
{
    printf("hello-cpp: C++ on OS101\n");

    bool ok = banner.constructed();
    ok = exercise_virtuals() && ok;
    ok = exercise_templates_and_new() && ok;
    ok = exercise_statics() && ok;
    printf("hello-cpp: self-test %s\n", ok ? "passed" : "FAILED");

    std::atexit(say_goodbye);

    uint64_t window = os101_window_create("Hello C++", 340, 180);
    if (window == OS101_SYS_ERROR) {
        printf("hello-cpp: the kernel refused a window\n");
        return 1;
    }

    os101_label_add(window, 16, 16, "C++ with virtuals, templates and RAII.");
    uint64_t status = os101_label_add(window, 16, 40,
                                      ok ? "runtime self-test: passed"
                                         : "runtime self-test: FAILED");
    uint64_t shape_label = os101_label_add(window, 16, 64, "");
    os101_button_add(window, 16, 100, 140, 30, "Next shape", kActionNext);
    os101_button_add(window, 168, 100, 140, 30, "Close", kActionQuit);
    os101_footer_set(window, "operator new, .init_array and guard variables all ran");

    // One of each concrete shape, held by base pointer, cycled by the button.
    Shape *shapes[3] = {new Circle(1.5), new Rectangle(2.0, 3.5),
                        new Triangle(4.0, 2.0)};
    int current = 0;
    char text[96];

    auto show = [&]() {
        std::snprintf(text, sizeof(text), "%s: area %.4f",
                      shapes[current]->name(), shapes[current]->area());
        os101_widget_update(window, shape_label, text);
    };
    show();

    for (;;) {
        os101_event ev = os101_event_poll(window);

        if (ev.kind == OS101_EVENT_CLOSED) {
            break;
        }
        if (ev.kind == OS101_EVENT_BUTTON) {
            if (ev.action_id == kActionQuit) {
                break;
            }
            current = (current + 1) % 3;
            show();
            std::snprintf(text, sizeof(text), "shape %d of 3, through a base pointer",
                          current + 1);
            os101_widget_update(window, status, text);
            continue;
        }
        // Cooperative scheduling: a poll loop has to give the CPU back.
        os101_yield();
    }

    for (Shape *shape : shapes) {
        delete shape;
    }
    printf("hello-cpp: leaving main\n");
    return ok ? 0 : 1;
}
