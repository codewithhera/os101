// The DOM, as seen from the page.
//
// This file is compiled into the kernel by `include_str!` and evaluated once
// into every page's engine, before any of the page's own script. Everything it
// reaches outside itself is one of the `__h_*` native functions registered by
// `browser/script.rs`, each of which takes and returns only numbers, strings and
// booleans — that is the whole width of the bridge, and the reason the wrappers,
// the node lists, the event objects and the timer queue are all built up here in
// JavaScript rather than handed over from Rust.
//
// A node is an integer on this side of the bridge: the `NodeId` the DOM assigned
// at parse time, or -1 for "there is no such node". -1 and -2 stand for the
// document and the window, which own event listeners but are not elements.
//
// The natives are deleted from the global object at the end, so a page cannot
// call them directly and cannot shadow them out from under this code.

(function (global) {
    'use strict';

    // ── the bridge ──────────────────────────────────────────────────────────

    var NATIVES = [
        '__h_doc_node', '__h_get_by_id', '__h_query_first', '__h_query_all',
        '__h_by_tag', '__h_by_class', '__h_matches', '__h_closest',
        '__h_title', '__h_set_title', '__h_ready_state',
        '__h_create_element', '__h_create_text',
        '__h_exists', '__h_node_type', '__h_tag',
        '__h_text', '__h_set_text', '__h_inner_text',
        '__h_inner_html', '__h_set_inner_html', '__h_outer_html', '__h_write',
        '__h_get_attr', '__h_set_attr', '__h_remove_attr', '__h_attr_names',
        '__h_parent', '__h_children', '__h_sibling', '__h_path',
        '__h_append', '__h_insert_before', '__h_remove_child',
        '__h_replace_child', '__h_detach', '__h_clone', '__h_contains',
        '__h_value', '__h_set_value', '__h_checked', '__h_set_checked',
        '__h_style_get', '__h_style_set', '__h_style_text', '__h_set_style_text',
        '__h_rect', '__h_log', '__h_alert', '__h_now_ms',
        '__h_navigate', '__h_location', '__h_cookie', '__h_set_cookie',
        '__h_storage', '__h_viewport', '__h_user_agent',
        '__h_focus', '__h_submit',
    ];

    var H = {};
    for (var i = 0; i < NATIVES.length; i++) {
        var name = NATIVES[i];
        var fn = global[name];
        // A missing native is a bug in script.rs, not in the page. Standing in a
        // thrower makes it say so at the call site instead of "not a function"
        // somewhere further along.
        H[name.slice(4)] = fn || (function (missing) {
            return function () { throw new Error('the kernel exposes no ' + missing); };
        })(name);
    }

    // Every native answers -1 for "there is no such node", so the document and
    // the window — which own listeners but are not elements — sit below that.
    var NONE = -1;
    var DOCUMENT = -2;
    var WINDOW = -3;

    /// A comma-separated list of node ids, as every list-returning native
    /// answers, turned into an array of numbers.
    function idList(text) {
        if (!text) return [];
        var parts = text.split(',');
        var out = [];
        for (var i = 0; i < parts.length; i++) {
            var id = +parts[i];
            if (id >= 0) out.push(id);
        }
        return out;
    }

    // ── node wrappers ───────────────────────────────────────────────────────

    // One wrapper per node id, so that `a === b` is true for two references to
    // the same element — which pages test, and which the previous engine got
    // wrong. The cache dies with the page, and node ids are never reused, so an
    // entry for a node a script has thrown away is a few dozen bytes and not a
    // stale answer.
    var wrappers = new Map();

    function wrap(id) {
        id = +id;
        if (!(id >= 0)) return null;
        var found = wrappers.get(id);
        if (found === undefined) {
            found = new Node(id);
            wrappers.set(id, found);
        }
        return found;
    }

    function wrapAll(text) {
        return idList(text).map(wrap);
    }

    /// The node id behind whatever a page passed us, or -1.
    function idOf(value) {
        if (value === document) return DOCUMENT;
        if (value === global) return WINDOW;
        if (value && typeof value === 'object' && typeof value.__id === 'number') {
            return value.__id;
        }
        return NONE;
    }

    function expectNode(value, method) {
        var id = idOf(value);
        if (id < 0) {
            throw new TypeError(method + ' expects a node');
        }
        return id;
    }

    // Node types, the same numbers every DOM uses.
    var ELEMENT_NODE = 1, TEXT_NODE = 3, DOCUMENT_NODE = 9;

    function Node(id) {
        // Not enumerable: a page that does `JSON.stringify(el)` or `{...el}`
        // should not find the kernel's handle in there.
        Object.defineProperty(this, '__id', { value: id });
    }

    /// Define `name` on Node.prototype from a getter and an optional setter,
    /// which is most of what an element is.
    function prop(name, get, set) {
        var descriptor = { enumerable: true, configurable: true, get: get };
        if (set) descriptor.set = set;
        Object.defineProperty(Node.prototype, name, descriptor);
    }

    /// An attribute a browser reflects as a property of the same name.
    function reflect(name, attribute) {
        prop(name,
            function () { var v = H.get_attr(this.__id, attribute); return v === null ? '' : v; },
            function (value) { H.set_attr(this.__id, attribute, String(value)); });
    }

    prop('nodeType', function () { return H.node_type(this.__id); });
    prop('tagName', function () {
        var tag = H.tag(this.__id);
        return tag ? tag.toUpperCase() : undefined;
    });
    prop('nodeName', function () {
        var tag = H.tag(this.__id);
        return tag ? tag.toUpperCase() : '#text';
    });
    prop('localName', function () { return H.tag(this.__id) || undefined; });

    prop('textContent',
        function () { return H.text(this.__id); },
        function (value) { H.set_text(this.__id, value === null ? '' : String(value)); });
    prop('innerText',
        function () { return H.inner_text(this.__id); },
        function (value) { H.set_text(this.__id, value === null ? '' : String(value)); });
    prop('nodeValue',
        function () { return this.nodeType === TEXT_NODE ? H.text(this.__id) : null; },
        function (value) { H.set_text(this.__id, value === null ? '' : String(value)); });
    prop('data',
        function () { return H.text(this.__id); },
        function (value) { H.set_text(this.__id, value === null ? '' : String(value)); });

    prop('innerHTML',
        function () { return H.inner_html(this.__id); },
        function (value) { H.set_inner_html(this.__id, value === null ? '' : String(value)); });
    prop('outerHTML', function () { return H.outer_html(this.__id); });

    prop('id',
        function () { var v = H.get_attr(this.__id, 'id'); return v === null ? '' : v; },
        function (value) { H.set_attr(this.__id, 'id', String(value)); });
    prop('className',
        function () { var v = H.get_attr(this.__id, 'class'); return v === null ? '' : v; },
        function (value) { H.set_attr(this.__id, 'class', String(value)); });

    reflect('href', 'href');
    reflect('src', 'src');
    reflect('alt', 'alt');
    reflect('title', 'title');
    reflect('type', 'type');
    reflect('name', 'name');
    reflect('placeholder', 'placeholder');
    reflect('action', 'action');
    reflect('method', 'method');
    reflect('rel', 'rel');
    reflect('target', 'target');

    prop('hidden',
        function () { return H.get_attr(this.__id, 'hidden') !== null; },
        function (value) {
            if (value) H.set_attr(this.__id, 'hidden', '');
            else H.remove_attr(this.__id, 'hidden');
        });
    prop('disabled',
        function () { return H.get_attr(this.__id, 'disabled') !== null; },
        function (value) {
            if (value) H.set_attr(this.__id, 'disabled', '');
            else H.remove_attr(this.__id, 'disabled');
        });

    // A field's value is what is in the field, which after the first keystroke
    // is no longer what the document said — the attribute is only where it
    // started. The host reads the control table first for exactly that reason.
    prop('value',
        function () { return H.value(this.__id); },
        function (value) { H.set_value(this.__id, value === null ? '' : String(value)); });
    prop('checked',
        function () { return H.checked(this.__id); },
        function (value) { H.set_checked(this.__id, !!value); });

    prop('parentNode', function () { return wrap(H.parent(this.__id)); });
    prop('parentElement', function () { return wrap(H.parent(this.__id)); });
    prop('childNodes', function () { return wrapAll(H.children(this.__id, false)); });
    prop('children', function () { return wrapAll(H.children(this.__id, true)); });
    prop('childElementCount', function () { return idList(H.children(this.__id, true)).length; });
    prop('firstChild', function () { return wrapAll(H.children(this.__id, false))[0] || null; });
    prop('lastChild', function () {
        var kids = wrapAll(H.children(this.__id, false));
        return kids[kids.length - 1] || null;
    });
    prop('firstElementChild', function () { return wrapAll(H.children(this.__id, true))[0] || null; });
    prop('lastElementChild', function () {
        var kids = wrapAll(H.children(this.__id, true));
        return kids[kids.length - 1] || null;
    });
    prop('nextSibling', function () { return wrap(H.sibling(this.__id, 1, false)); });
    prop('previousSibling', function () { return wrap(H.sibling(this.__id, -1, false)); });
    prop('nextElementSibling', function () { return wrap(H.sibling(this.__id, 1, true)); });
    prop('previousElementSibling', function () { return wrap(H.sibling(this.__id, -1, true)); });
    prop('ownerDocument', function () { return document; });
    prop('isConnected', function () { return H.exists(this.__id); });

    prop('style', function () { return styleFor(this.__id); });
    prop('classList', function () { return classListFor(this.__id); });
    prop('dataset', function () { return datasetFor(this.__id); });
    prop('attributes', function () {
        var id = this.__id;
        return H.attr_names(id).split(',').filter(Boolean).map(function (name) {
            return { name: name, value: H.get_attr(id, name) };
        });
    });

    // Geometry comes from the layout tree, which means the layout has to be up
    // to date: the host lays the page out again here if a script has changed it
    // since the last one. That is the one place a mutation is not batched, and
    // it is the same trade a real browser makes.
    prop('offsetWidth', function () { return rectOf(this.__id).width; });
    prop('offsetHeight', function () { return rectOf(this.__id).height; });
    prop('clientWidth', function () { return rectOf(this.__id).width; });
    prop('clientHeight', function () { return rectOf(this.__id).height; });
    prop('offsetTop', function () { return rectOf(this.__id).top; });
    prop('offsetLeft', function () { return rectOf(this.__id).left; });
    prop('offsetParent', function () { return wrap(H.parent(this.__id)); });
    prop('scrollTop', function () { return 0; }, function () {});
    prop('scrollLeft', function () { return 0; }, function () {});
    prop('scrollWidth', function () { return rectOf(this.__id).width; });
    prop('scrollHeight', function () { return rectOf(this.__id).height; });

    function rectOf(id) {
        var parts = H.rect(id).split(',');
        var x = +parts[0] || 0, y = +parts[1] || 0;
        var w = +parts[2] || 0, h = +parts[3] || 0;
        return {
            x: x, y: y, left: x, top: y, right: x + w, bottom: y + h,
            width: w, height: h,
            toJSON: function () { return { x: x, y: y, width: w, height: h }; },
        };
    }

    Node.prototype.getBoundingClientRect = function () { return rectOf(this.__id); };
    Node.prototype.getClientRects = function () { return [rectOf(this.__id)]; };

    Node.prototype.getAttribute = function (name) {
        return H.get_attr(this.__id, String(name));
    };
    Node.prototype.setAttribute = function (name, value) {
        H.set_attr(this.__id, String(name), value === undefined ? 'undefined' : String(value));
    };
    Node.prototype.removeAttribute = function (name) {
        H.remove_attr(this.__id, String(name));
    };
    Node.prototype.hasAttribute = function (name) {
        return H.get_attr(this.__id, String(name)) !== null;
    };
    Node.prototype.getAttributeNames = function () {
        return H.attr_names(this.__id).split(',').filter(Boolean);
    };
    Node.prototype.toggleAttribute = function (name, force) {
        var on = force === undefined ? !this.hasAttribute(name) : !!force;
        if (on) this.setAttribute(name, '');
        else this.removeAttribute(name);
        return on;
    };

    Node.prototype.appendChild = function (child) {
        var id = expectNode(child, 'appendChild');
        if (child.__fragment) return moveFragment(this, child, null);
        if (!H.append(this.__id, id)) {
            throw new Error('appendChild could not attach that node');
        }
        return child;
    };

    /// A document fragment is a real `<div>` here, because the DOM in Rust has no
    /// notion of a node that is not an element. Attaching one therefore has to
    /// move its children and leave the div behind — otherwise a page that builds
    /// its rows in a fragment and appends it to a table would get a `<div>` in
    /// the middle of the table, and the table would come apart.
    function moveFragment(parent, fragment, reference) {
        var moving = fragment.childNodes;
        for (var i = 0; i < moving.length; i++) {
            if (reference) H.insert_before(parent.__id, moving[i].__id, reference.__id);
            else H.append(parent.__id, moving[i].__id);
        }
        return fragment;
    }
    Node.prototype.append = function () {
        for (var i = 0; i < arguments.length; i++) {
            var value = arguments[i];
            this.appendChild(typeof value === 'string' ? document.createTextNode(value) : value);
        }
    };
    Node.prototype.insertBefore = function (child, reference) {
        var id = expectNode(child, 'insertBefore');
        if (child.__fragment) return moveFragment(this, child, reference || null);
        // A null reference means "at the end", which is what the DOM says.
        if (!H.insert_before(this.__id, id, reference == null ? NONE : idOf(reference))) {
            throw new Error('insertBefore could not attach that node');
        }
        return child;
    };
    Node.prototype.removeChild = function (child) {
        var id = expectNode(child, 'removeChild');
        if (!H.remove_child(this.__id, id)) {
            throw new Error('removeChild was given a node that is not a child');
        }
        return child;
    };
    Node.prototype.replaceChild = function (fresh, stale) {
        var newId = expectNode(fresh, 'replaceChild');
        var oldId = expectNode(stale, 'replaceChild');
        if (!H.replace_child(this.__id, newId, oldId)) {
            throw new Error('replaceChild was given a node that is not a child');
        }
        return stale;
    };
    Node.prototype.remove = function () { H.detach(this.__id); };
    Node.prototype.cloneNode = function (deep) { return wrap(H.clone(this.__id, !!deep)); };
    Node.prototype.contains = function (other) {
        var id = idOf(other);
        return id >= 0 && H.contains(this.__id, id);
    };
    Node.prototype.hasChildNodes = function () {
        return idList(H.children(this.__id, false)).length > 0;
    };
    Node.prototype.querySelector = function (selector) {
        return wrap(H.query_first(this.__id, String(selector)));
    };
    Node.prototype.querySelectorAll = function (selector) {
        return wrapAll(H.query_all(this.__id, String(selector)));
    };
    Node.prototype.getElementsByTagName = function (tag) {
        return wrapAll(H.by_tag(this.__id, String(tag)));
    };
    Node.prototype.getElementsByClassName = function (name) {
        return wrapAll(H.by_class(this.__id, String(name)));
    };
    Node.prototype.matches = function (selector) {
        return H.matches(this.__id, String(selector));
    };
    Node.prototype.closest = function (selector) {
        return wrap(H.closest(this.__id, String(selector)));
    };
    Node.prototype.focus = function () { H.focus(this.__id, true); };
    Node.prototype.blur = function () { H.focus(this.__id, false); };
    Node.prototype.scrollIntoView = function () {};
    Node.prototype.submit = function () { H.submit(this.__id); };
    Node.prototype.insertAdjacentHTML = function (where, html) {
        var place = String(where).toLowerCase();
        var holder = document.createElement('div');
        holder.innerHTML = String(html);
        var moving = holder.childNodes;
        var parent = this.parentNode;
        for (var i = 0; i < moving.length; i++) {
            if (place === 'beforeend') this.appendChild(moving[i]);
            else if (place === 'afterbegin') this.insertBefore(moving[i], this.firstChild);
            else if (place === 'beforebegin' && parent) parent.insertBefore(moving[i], this);
            else if (place === 'afterend' && parent) parent.insertBefore(moving[i], this.nextSibling);
        }
    };
    Node.prototype.toString = function () {
        var tag = H.tag(this.__id);
        return tag ? '[object HTML' + tag.toUpperCase() + 'Element]' : '[object Text]';
    };

    // ── element.classList ───────────────────────────────────────────────────

    function DOMTokenList(id) {
        Object.defineProperty(this, '__id', { value: id });
    }

    DOMTokenList.prototype.__tokens = function () {
        var value = H.get_attr(this.__id, 'class');
        return value ? value.split(/\s+/).filter(Boolean) : [];
    };
    DOMTokenList.prototype.__write = function (tokens) {
        H.set_attr(this.__id, 'class', tokens.join(' '));
    };
    DOMTokenList.prototype.contains = function (name) {
        return this.__tokens().indexOf(String(name)) >= 0;
    };
    DOMTokenList.prototype.add = function () {
        var tokens = this.__tokens();
        for (var i = 0; i < arguments.length; i++) {
            var name = String(arguments[i]).trim();
            if (name && tokens.indexOf(name) < 0) tokens.push(name);
        }
        this.__write(tokens);
    };
    DOMTokenList.prototype.remove = function () {
        var gone = [];
        for (var i = 0; i < arguments.length; i++) gone.push(String(arguments[i]).trim());
        this.__write(this.__tokens().filter(function (t) { return gone.indexOf(t) < 0; }));
    };
    DOMTokenList.prototype.toggle = function (name, force) {
        name = String(name).trim();
        var on = force === undefined ? !this.contains(name) : !!force;
        if (on) this.add(name); else this.remove(name);
        return on;
    };
    DOMTokenList.prototype.replace = function (from, to) {
        if (!this.contains(from)) return false;
        this.remove(from);
        this.add(to);
        return true;
    };
    DOMTokenList.prototype.item = function (index) {
        var tokens = this.__tokens();
        return index >= 0 && index < tokens.length ? tokens[index] : null;
    };
    DOMTokenList.prototype.forEach = function (fn, self) {
        this.__tokens().forEach(fn, self);
    };
    DOMTokenList.prototype.toString = function () { return this.__tokens().join(' '); };
    Object.defineProperty(DOMTokenList.prototype, 'length', {
        get: function () { return this.__tokens().length; },
    });
    Object.defineProperty(DOMTokenList.prototype, 'value', {
        get: function () { return this.__tokens().join(' '); },
        set: function (v) { H.set_attr(this.__id, 'class', String(v)); },
    });
    DOMTokenList.prototype[Symbol.iterator] = function () {
        return this.__tokens()[Symbol.iterator]();
    };

    var classLists = new Map();
    function classListFor(id) {
        var found = classLists.get(id);
        if (!found) {
            found = new DOMTokenList(id);
            classLists.set(id, found);
        }
        return found;
    }

    // ── element.style ───────────────────────────────────────────────────────

    // A Proxy rather than a fixed list of accessors, because there is no useful
    // place to stop: a page setting `style.gridTemplateColumns` should have the
    // declaration land in the style attribute even though this engine's CSS will
    // ignore it, otherwise the assignment silently disappears and the page reads
    // its own style back as empty.
    var styles = new Map();

    function cssName(key) {
        if (key.indexOf('-') >= 0) return key.toLowerCase();
        return key.replace(/[A-Z]/g, function (c) { return '-' + c.toLowerCase(); });
    }

    function styleFor(id) {
        var found = styles.get(id);
        if (found) return found;

        var methods = {
            setProperty: function (name, value) { H.style_set(id, cssName(String(name)), String(value)); },
            removeProperty: function (name) {
                var was = H.style_get(id, cssName(String(name)));
                H.style_set(id, cssName(String(name)), '');
                return was;
            },
            getPropertyValue: function (name) { return H.style_get(id, cssName(String(name))); },
            getPropertyPriority: function () { return ''; },
            item: function () { return ''; },
        };

        found = new Proxy(methods, {
            get: function (target, key) {
                if (typeof key !== 'string') return target[key];
                if (key === 'cssText') return H.style_text(id);
                if (key === 'length') return 0;
                if (key in target) return target[key];
                return H.style_get(id, cssName(key));
            },
            set: function (target, key, value) {
                if (typeof key !== 'string') return false;
                if (key === 'cssText') H.set_style_text(id, String(value));
                else H.style_set(id, cssName(key), value == null ? '' : String(value));
                return true;
            },
            has: function (target, key) { return typeof key === 'string'; },
        });
        styles.set(id, found);
        return found;
    }

    // ── element.dataset ─────────────────────────────────────────────────────

    var datasets = new Map();

    function datasetFor(id) {
        var found = datasets.get(id);
        if (found) return found;
        found = new Proxy({}, {
            get: function (target, key) {
                if (typeof key !== 'string') return undefined;
                var value = H.get_attr(id, 'data-' + cssName(key));
                return value === null ? undefined : value;
            },
            set: function (target, key, value) {
                if (typeof key !== 'string') return false;
                H.set_attr(id, 'data-' + cssName(key), String(value));
                return true;
            },
            deleteProperty: function (target, key) {
                H.remove_attr(id, 'data-' + cssName(key));
                return true;
            },
            has: function (target, key) {
                return typeof key === 'string' && H.get_attr(id, 'data-' + cssName(key)) !== null;
            },
        });
        datasets.set(id, found);
        return found;
    }

    // ── events ──────────────────────────────────────────────────────────────

    // Which events bubble, which is what decides whether a listener on an
    // ancestor hears about one. `focus` and `load` famously do not.
    var NON_BUBBLING = { load: 1, unload: 1, focus: 1, blur: 1, error: 1, abort: 1 };

    function DOMEvent(type, init) {
        init = init || {};
        this.type = String(type);
        this.bubbles = init.bubbles !== undefined ? !!init.bubbles : !NON_BUBBLING[this.type];
        this.cancelable = init.cancelable !== undefined ? !!init.cancelable : true;
        this.detail = init.detail !== undefined ? init.detail : null;
        this.target = null;
        this.currentTarget = null;
        this.eventPhase = 0;
        this.defaultPrevented = false;
        this.timeStamp = H.now_ms();
        this.isTrusted = !!init.isTrusted;
        this.__stopped = false;
        this.__stoppedNow = false;
        // Keyboard and mouse detail, when the host supplied any. Absent
        // properties read as undefined, which is what a page checking for them
        // will see in a real browser for an event of the wrong kind.
        if (init.key !== undefined) this.key = init.key;
        if (init.keyCode !== undefined) {
            this.keyCode = init.keyCode;
            this.which = init.keyCode;
            this.charCode = init.keyCode;
        }
        if (init.clientX !== undefined) {
            this.clientX = init.clientX;
            this.pageX = init.clientX;
            this.offsetX = init.clientX;
        }
        if (init.clientY !== undefined) {
            this.clientY = init.clientY;
            this.pageY = init.clientY;
            this.offsetY = init.clientY;
        }
        this.altKey = !!init.altKey;
        this.ctrlKey = !!init.ctrlKey;
        this.shiftKey = !!init.shiftKey;
        this.metaKey = !!init.metaKey;
    }

    DOMEvent.prototype.preventDefault = function () {
        if (this.cancelable) this.defaultPrevented = true;
    };
    DOMEvent.prototype.stopPropagation = function () { this.__stopped = true; };
    DOMEvent.prototype.stopImmediatePropagation = function () {
        this.__stopped = true;
        this.__stoppedNow = true;
    };
    DOMEvent.prototype.composedPath = function () { return []; };
    Object.defineProperty(DOMEvent.prototype, 'srcElement', {
        get: function () { return this.target; },
    });
    Object.defineProperty(DOMEvent.prototype, 'returnValue', {
        get: function () { return !this.defaultPrevented; },
        set: function (v) { if (!v) this.preventDefault(); },
    });

    // id -> [{ type, fn, capture, once }]
    var listeners = new Map();
    // id -> { type: fn }, for the `el.onclick = f` form, which is one handler
    // per type rather than a list.
    var handlers = new Map();
    // Compiled `onclick="..."` attributes, keyed by their own source so that a
    // handler firing a hundred times is compiled once.
    var compiled = new Map();

    var MAX_LISTENERS = 2048;
    var listenerCount = 0;

    function addListener(id, type, fn, options) {
        if (typeof fn !== 'function' || id === NONE) return;
        if (listenerCount >= MAX_LISTENERS) return;
        var capture = false, once = false;
        if (options === true) capture = true;
        else if (options && typeof options === 'object') {
            capture = !!options.capture;
            once = !!options.once;
        }
        type = String(type);
        var list = listeners.get(id);
        if (!list) { list = []; listeners.set(id, list); }
        // The DOM ignores a repeat registration of the same function in the same
        // phase, and pages lean on that to stay idempotent.
        for (var i = 0; i < list.length; i++) {
            if (list[i].type === type && list[i].fn === fn && list[i].capture === capture) return;
        }
        list.push({ type: type, fn: fn, capture: capture, once: once });
        listenerCount++;
    }

    function removeListener(id, type, fn, options) {
        var list = listeners.get(id);
        if (!list) return;
        // Coerced, not just short-circuited: `undefined` from a missing options
        // argument would never match the `false` that `addListener` stored, and
        // `removeEventListener` would silently do nothing.
        var capture = options === true
            || !!(options && typeof options === 'object' && options.capture);
        type = String(type);
        for (var i = 0; i < list.length; i++) {
            if (list[i].type === type && list[i].fn === fn && list[i].capture === capture) {
                list.splice(i, 1);
                listenerCount--;
                return;
            }
        }
    }

    Node.prototype.addEventListener = function (type, fn, options) {
        addListener(this.__id, type, fn, options);
    };
    Node.prototype.removeEventListener = function (type, fn, options) {
        removeListener(this.__id, type, fn, options);
    };
    Node.prototype.dispatchEvent = function (event) {
        dispatchOn(this.__id, event);
        return !event.defaultPrevented;
    };
    Node.prototype.click = function () {
        dispatchOn(this.__id, new DOMEvent('click', { bubbles: true }));
    };

    // `el.onclick = f` for every event the browser can raise, plus the ones a
    // page sets on the window.
    var HANDLER_PROPS = [
        'click', 'dblclick', 'mousedown', 'mouseup', 'mouseover', 'mouseout',
        'mousemove', 'input', 'change', 'submit', 'reset', 'focus', 'blur',
        'keydown', 'keyup', 'keypress', 'load', 'unload', 'scroll', 'error',
        'contextmenu', 'DOMContentLoaded', 'readystatechange', 'hashchange',
    ];

    function defineHandlerProp(target, type) {
        Object.defineProperty(target, 'on' + type, {
            configurable: true,
            get: function () {
                var slot = handlers.get(idOf(this));
                return (slot && slot[type]) || null;
            },
            set: function (fn) {
                var id = idOf(this);
                if (id === NONE) return;
                var slot = handlers.get(id);
                if (!slot) { slot = {}; handlers.set(id, slot); }
                slot[type] = typeof fn === 'function' ? fn : null;
            },
        });
    }

    for (var h = 0; h < HANDLER_PROPS.length; h++) {
        defineHandlerProp(Node.prototype, HANDLER_PROPS[h]);
    }

    /// The `on<type>` attribute of `id`, compiled once and cached.
    function attributeHandler(id, type) {
        var source = H.get_attr(id, 'on' + type);
        if (source === null || !source.trim()) return null;
        var key = type + '\u0000' + source;
        var fn = compiled.get(key);
        if (fn === undefined) {
            try {
                // Sloppy mode and a bare `event` binding, which is what an
                // attribute handler gets in a browser.
                fn = new Function('event', source);
            } catch (e) {
                report('error', 'on' + type + ' attribute: ' + e);
                fn = null;
            }
            compiled.set(key, fn);
        }
        return fn;
    }

    var ran = 0;

    function invoke(fn, node, id, event) {
        event.currentTarget = node;
        ran++;
        try {
            fn.call(node, event);
        } catch (e) {
            report('error', 'in a ' + event.type + ' handler: ' + describeError(e));
        }
    }

    function targetFor(id) {
        if (id === DOCUMENT) return document;
        if (id === WINDOW) return global;
        return wrap(id);
    }

    /// Run everything registered on one node for one event, in the phase the
    /// caller is in.
    function fireAt(id, event, capturing) {
        var node = targetFor(id);
        if (!node) return;

        if (!capturing) {
            // An attribute handler was registered when the document was parsed,
            // so it goes ahead of anything a script added later.
            var attribute = id >= 0 ? attributeHandler(id, event.type) : null;
            if (attribute) invoke(attribute, node, id, event);
            if (event.__stoppedNow) return;

            var slot = handlers.get(id);
            if (slot && slot[event.type]) invoke(slot[event.type], node, id, event);
            if (event.__stoppedNow) return;
        }

        var list = listeners.get(id);
        if (!list) return;
        // A copy, because a handler may add or remove listeners while running.
        var due = list.slice();
        for (var i = 0; i < due.length; i++) {
            var entry = due[i];
            if (entry.type !== event.type || entry.capture !== capturing) continue;
            if (entry.once) removeListener(id, entry.type, entry.fn, entry.capture);
            invoke(entry.fn, node, id, event);
            if (event.__stoppedNow) return;
        }
    }

    /// Dispatch `event` at `id`, through the capture, target and bubble phases.
    function dispatchOn(id, event) {
        // The path is resolved up front: a handler may restructure the tree, and
        // the event should still visit the ancestors it started among.
        var path;
        if (id === WINDOW) path = [WINDOW];
        else if (id === DOCUMENT) path = [DOCUMENT, WINDOW];
        else path = idList(H.path(id)).concat([DOCUMENT, WINDOW]);

        event.target = targetFor(id);

        // Capture: outermost inward, not including the target.
        event.eventPhase = 1;
        for (var i = path.length - 1; i >= 1; i--) {
            fireAt(path[i], event, true);
            if (event.__stopped) { finish(event); return event; }
        }

        // At the target, where both phases' listeners run.
        event.eventPhase = 2;
        fireAt(path[0], event, true);
        if (!event.__stoppedNow) fireAt(path[0], event, false);
        if (event.__stopped) { finish(event); return event; }

        // Bubble: inward out.
        if (event.bubbles) {
            event.eventPhase = 3;
            for (var j = 1; j < path.length; j++) {
                fireAt(path[j], event, false);
                if (event.__stopped) break;
            }
        }
        finish(event);
        return event;
    }

    function finish(event) {
        event.eventPhase = 0;
        event.currentTarget = null;
    }

    // ── timers ──────────────────────────────────────────────────────────────

    var timers = new Map();
    var nextTimer = 1;
    var frames = [];
    var MAX_TIMERS = 256;
    /// How many timer callbacks one pump will run before leaving the rest for
    /// the next one. A `setTimeout(f, 0)` that reschedules itself is an ordinary
    /// thing for a page to do; running it until it stops would be a hang.
    var MAX_PER_PUMP = 64;

    function schedule(fn, delay, every, args) {
        if (typeof fn === 'string') {
            // `setTimeout('code')` is ancient but pages still ship it.
            var source = fn;
            fn = function () { evaluate(source); };
        }
        if (typeof fn !== 'function') return 0;
        if (timers.size >= MAX_TIMERS) return 0;
        delay = +delay;
        if (!(delay >= 0)) delay = 0;
        var id = nextTimer++;
        timers.set(id, {
            fn: fn,
            args: args,
            due: H.now_ms() + delay,
            every: every ? Math.max(delay, 4) : 0,
        });
        return id;
    }

    global.setTimeout = function (fn, delay) {
        return schedule(fn, delay, false, Array.prototype.slice.call(arguments, 2));
    };
    global.setInterval = function (fn, delay) {
        return schedule(fn, delay, true, Array.prototype.slice.call(arguments, 2));
    };
    global.clearTimeout = function (id) { timers.delete(+id); };
    global.clearInterval = global.clearTimeout;
    global.queueMicrotask = function (fn) { Promise.resolve().then(fn); };
    global.requestAnimationFrame = function (fn) {
        if (typeof fn !== 'function' || frames.length >= MAX_TIMERS) return 0;
        var id = nextTimer++;
        frames.push({ id: id, fn: fn });
        return id;
    };
    global.cancelAnimationFrame = function (id) {
        frames = frames.filter(function (f) { return f.id !== +id; });
    };

    /// Run whatever is due. Called by the browser's event loop with the same
    /// clock `H.now_ms` reads, so a timer set for 100 ms fires 100 ms later
    /// rather than on the next pass.
    function runDue(now) {
        var count = 0;

        var due = [];
        timers.forEach(function (timer, id) {
            if (timer.due <= now) due.push({ id: id, timer: timer });
        });
        // Earliest first, and by id when two are due together, which is the
        // order the DOM promises.
        due.sort(function (a, b) { return (a.timer.due - b.timer.due) || (a.id - b.id); });

        for (var i = 0; i < due.length && count < MAX_PER_PUMP; i++) {
            var entry = due[i];
            if (!timers.has(entry.id)) continue;
            if (entry.timer.every) entry.timer.due = now + entry.timer.every;
            else timers.delete(entry.id);
            count++;
            try {
                entry.timer.fn.apply(global, entry.timer.args || []);
            } catch (e) {
                report('error', 'in a timer: ' + describeError(e));
            }
        }

        if (frames.length) {
            // Taken before any of them runs: a callback that asks for another
            // frame belongs to the next pump, not this one, or an animation would
            // never give the rest of the loop a turn.
            var queued = frames;
            frames = [];
            for (var f = 0; f < queued.length; f++) {
                count++;
                try {
                    queued[f].fn(now);
                } catch (e) {
                    report('error', 'in a requestAnimationFrame callback: ' + describeError(e));
                }
            }
        }
        return count;
    }

    /// Whether anything is waiting, so the browser knows to keep pumping.
    function pending() {
        return timers.size > 0 || frames.length > 0;
    }

    // ── console ─────────────────────────────────────────────────────────────

    function describeError(e) {
        if (e instanceof Error) {
            return (e.name || 'Error') + ': ' + e.message;
        }
        return show(e, 0);
    }

    /// Format a value the way a console does: strings bare at the top level,
    /// objects and arrays inspected a couple of levels deep. Depth is bounded
    /// because a DOM is full of cycles and this runs on a kernel stack.
    function show(value, depth) {
        if (typeof value === 'string') return depth === 0 ? value : JSON.stringify(value);
        if (value === null) return 'null';
        if (value === undefined) return 'undefined';
        if (typeof value === 'number' || typeof value === 'boolean') return String(value);
        if (typeof value === 'bigint') return String(value) + 'n';
        if (typeof value === 'symbol') return value.toString();
        if (typeof value === 'function') {
            return '[Function' + (value.name ? ': ' + value.name : '') + ']';
        }
        if (value instanceof Error) return describeError(value);
        if (value instanceof Node) return value.toString();
        if (depth > 2) return Array.isArray(value) ? '[Array]' : '[Object]';

        if (Array.isArray(value)) {
            var items = [];
            for (var i = 0; i < value.length && i < 32; i++) items.push(show(value[i], depth + 1));
            if (value.length > 32) items.push('... ' + (value.length - 32) + ' more');
            return '[' + items.join(', ') + ']';
        }
        if (value instanceof Map) return 'Map(' + value.size + ')';
        if (value instanceof Set) return 'Set(' + value.size + ')';
        if (value instanceof Date) return value.toISOString();
        if (value instanceof RegExp) return value.toString();
        if (typeof value.then === 'function') return '[Promise]';

        var fields = [];
        var keys = Object.keys(value);
        for (var k = 0; k < keys.length && k < 24; k++) {
            var text;
            try {
                text = show(value[keys[k]], depth + 1);
            } catch (e) {
                text = '[throws]';
            }
            fields.push(keys[k] + ': ' + text);
        }
        if (keys.length > 24) fields.push('... ' + (keys.length - 24) + ' more');
        return '{ ' + fields.join(', ') + ' }';
    }

    function format(args) {
        var parts = [];
        for (var i = 0; i < args.length; i++) parts.push(show(args[i], 0));
        return parts.join(' ');
    }

    function report(level, message) {
        H.log(level, message);
    }

    function logger(level) {
        return function () { H.log(level, format(arguments)); };
    }

    var groupDepth = 0;
    global.console = {
        log: logger('log'),
        info: logger('info'),
        warn: logger('warn'),
        error: logger('error'),
        debug: logger('debug'),
        trace: logger('debug'),
        dir: logger('log'),
        table: logger('log'),
        assert: function (ok) {
            if (!ok) H.log('error', 'assertion failed: ' + format(Array.prototype.slice.call(arguments, 1)));
        },
        group: function () { groupDepth++; H.log('log', format(arguments)); },
        groupEnd: function () { if (groupDepth > 0) groupDepth--; },
        count: function () {},
        time: function () {},
        timeEnd: function () {},
        clear: function () {},
    };

    // ── document ────────────────────────────────────────────────────────────

    var document = Object.create(Object.prototype);

    function docProp(name, get, set) {
        var descriptor = { enumerable: true, configurable: true, get: get };
        if (set) descriptor.set = set;
        Object.defineProperty(document, name, descriptor);
    }

    docProp('nodeType', function () { return DOCUMENT_NODE; });
    docProp('nodeName', function () { return '#document'; });
    docProp('documentElement', function () { return wrap(H.doc_node(1)); });
    docProp('body', function () { return wrap(H.doc_node(2)); });
    docProp('head', function () { return wrap(H.doc_node(3)); });
    docProp('title',
        function () { return H.title(); },
        function (value) { H.set_title(String(value)); });
    docProp('readyState', function () { return H.ready_state(); });
    docProp('URL', function () { return H.location('href'); });
    docProp('documentURI', function () { return H.location('href'); });
    docProp('referrer', function () { return ''; });
    docProp('characterSet', function () { return 'UTF-8'; });
    docProp('compatMode', function () { return 'CSS1Compat'; });
    docProp('defaultView', function () { return global; });
    docProp('scrollingElement', function () { return wrap(H.doc_node(2)); });
    docProp('activeElement', function () { return wrap(H.doc_node(2)); });
    docProp('forms', function () { return wrapAll(H.by_tag(H.doc_node(0), 'form')); });
    docProp('images', function () { return wrapAll(H.by_tag(H.doc_node(0), 'img')); });
    docProp('links', function () { return wrapAll(H.by_tag(H.doc_node(0), 'a')); });
    docProp('scripts', function () { return wrapAll(H.by_tag(H.doc_node(0), 'script')); });
    docProp('children', function () { return wrapAll(H.children(H.doc_node(0), true)); });
    docProp('childNodes', function () { return wrapAll(H.children(H.doc_node(0), false)); });
    docProp('firstChild', function () { return wrapAll(H.children(H.doc_node(0), false))[0] || null; });
    // The store is per-page and goes no further: nothing puts it in a request
    // header, and nothing writes it to the disk.
    docProp('cookie',
        function () { return H.cookie(); },
        function (value) { H.set_cookie(String(value)); });

    document.getElementById = function (name) {
        return wrap(H.get_by_id(String(name)));
    };
    document.querySelector = function (selector) {
        return wrap(H.query_first(H.doc_node(0), String(selector)));
    };
    document.querySelectorAll = function (selector) {
        return wrapAll(H.query_all(H.doc_node(0), String(selector)));
    };
    document.getElementsByTagName = function (tag) {
        return wrapAll(H.by_tag(H.doc_node(0), String(tag)));
    };
    document.getElementsByClassName = function (name) {
        return wrapAll(H.by_class(H.doc_node(0), String(name)));
    };
    document.getElementsByName = function (name) {
        return wrapAll(H.query_all(H.doc_node(0), '[name="' + String(name).replace(/"/g, '') + '"]'));
    };
    document.createElement = function (tag) {
        var id = H.create_element(String(tag));
        if (id < 0) throw new Error('createElement could not make a ' + tag);
        return wrap(id);
    };
    document.createTextNode = function (text) {
        return wrap(H.create_text(text === undefined ? '' : String(text)));
    };
    document.createComment = function () { return wrap(H.create_text('')); };
    document.createDocumentFragment = function () {
        var holder = document.createElement('div');
        // Marked rather than a class of its own, so that everything else about it
        // — appendChild, querySelector, innerHTML — is an ordinary element.
        Object.defineProperty(holder, '__fragment', { value: true });
        return holder;
    };
    document.createEvent = function (kind) { return new DOMEvent(String(kind).toLowerCase()); };
    document.addEventListener = function (type, fn, options) {
        addListener(DOCUMENT, type, fn, options);
    };
    document.removeEventListener = function (type, fn, options) {
        removeListener(DOCUMENT, type, fn, options);
    };
    document.dispatchEvent = function (event) {
        dispatchOn(DOCUMENT, event);
        return !event.defaultPrevented;
    };
    document.contains = function (node) {
        var id = idOf(node);
        return id >= 0 && H.exists(id);
    };
    // The parser has already finished by the time any of this runs, so a write
    // cannot go into the stream. What it can do is land where the stream would
    // have been: right after the `<script>` element doing the writing. Outside a
    // load-time script there is no such place, and a real browser would replace
    // the whole document — so that case is reported rather than obeyed.
    document.write = function () {
        var html = Array.prototype.join.call(arguments, '');
        if (!H.write(html)) {
            H.log('warn', 'document.write outside a load-time script, ignored: ' + html);
        }
    };
    document.writeln = function () {
        document.write(Array.prototype.join.call(arguments, '') + '\n');
    };
    document.open = function () {};
    document.close = function () {};
    document.execCommand = function () { return false; };

    for (var d = 0; d < HANDLER_PROPS.length; d++) {
        defineHandlerProp(document, HANDLER_PROPS[d]);
    }

    global.document = document;

    // ── window ──────────────────────────────────────────────────────────────

    global.window = global;
    global.self = global;
    global.top = global;
    global.parent = global;
    global.frames = global;
    global.length = 0;
    global.closed = false;
    global.name = '';
    global.isSecureContext = true;
    global.origin = H.location('origin');

    Object.defineProperty(global, 'innerWidth', { get: function () { return H.viewport(0); } });
    Object.defineProperty(global, 'innerHeight', { get: function () { return H.viewport(1); } });
    Object.defineProperty(global, 'outerWidth', { get: function () { return H.viewport(0); } });
    Object.defineProperty(global, 'outerHeight', { get: function () { return H.viewport(1); } });
    Object.defineProperty(global, 'devicePixelRatio', { get: function () { return 1; } });
    Object.defineProperty(global, 'scrollX', { get: function () { return 0; } });
    Object.defineProperty(global, 'scrollY', { get: function () { return 0; } });
    Object.defineProperty(global, 'pageXOffset', { get: function () { return 0; } });
    Object.defineProperty(global, 'pageYOffset', { get: function () { return 0; } });

    global.addEventListener = function (type, fn, options) { addListener(WINDOW, type, fn, options); };
    global.removeEventListener = function (type, fn, options) { removeListener(WINDOW, type, fn, options); };
    global.dispatchEvent = function (event) {
        dispatchOn(WINDOW, event);
        return !event.defaultPrevented;
    };
    for (var w = 0; w < HANDLER_PROPS.length; w++) {
        defineHandlerProp(global, HANDLER_PROPS[w]);
    }

    global.alert = function (message) { H.alert(message === undefined ? '' : String(message)); };
    // There is no modal to answer with, so a question is refused rather than
    // guessed at.
    global.confirm = function (message) { H.alert(message === undefined ? '' : String(message)); return false; };
    global.prompt = function (message) { H.alert(message === undefined ? '' : String(message)); return null; };
    global.print = function () {};
    global.focus = function () {};
    global.blur = function () {};
    global.scroll = function () {};
    global.scrollTo = function () {};
    global.scrollBy = function () {};
    global.resizeTo = function () {};
    global.moveTo = function () {};
    global.close = function () {};
    global.open = function (url) {
        if (url) H.navigate(String(url));
        return null;
    };
    global.getSelection = function () { return null; };
    global.matchMedia = function (query) {
        return {
            media: String(query),
            matches: false,
            addListener: function () {},
            removeListener: function () {},
            addEventListener: function () {},
            removeEventListener: function () {},
        };
    };
    // Only the inline declarations, which is all this engine can tell a page
    // about: the cascade lives in Rust and is thrown away after each layout.
    global.getComputedStyle = function (element) {
        var id = idOf(element);
        if (id < 0) throw new TypeError('getComputedStyle expects an element');
        return styleFor(id);
    };
    global.performance = {
        now: function () { return H.now_ms(); },
        timeOrigin: 0,
        mark: function () {},
        measure: function () {},
        getEntriesByName: function () { return []; },
    };
    global.navigator = {
        userAgent: H.user_agent(),
        appVersion: H.user_agent(),
        appName: 'Netscape',
        platform: 'OS101 x86_64',
        vendor: '',
        language: 'en-US',
        languages: ['en-US', 'en'],
        onLine: true,
        cookieEnabled: true,
        doNotTrack: null,
        hardwareConcurrency: 1,
        maxTouchPoints: 0,
        javaEnabled: function () { return false; },
        sendBeacon: function () { return false; },
    };
    global.screen = {
        get width() { return H.viewport(0); },
        get height() { return H.viewport(1); },
        get availWidth() { return H.viewport(0); },
        get availHeight() { return H.viewport(1); },
        colorDepth: 24,
        pixelDepth: 24,
    };

    // ── location ────────────────────────────────────────────────────────────

    var location = {};
    var PARTS = ['href', 'protocol', 'host', 'hostname', 'port', 'pathname', 'search', 'hash', 'origin'];
    for (var p = 0; p < PARTS.length; p++) {
        (function (part) {
            Object.defineProperty(location, part, {
                enumerable: true,
                get: function () { return H.location(part); },
                // Assigning to any part of a location navigates, and the only
                // part this browser can resolve on its own is the whole address.
                set: function (value) { H.navigate(String(value)); },
            });
        })(PARTS[p]);
    }
    location.assign = function (url) { H.navigate(String(url)); };
    location.replace = function (url) { H.navigate(String(url)); };
    location.reload = function () { H.navigate(H.location('href')); };
    location.toString = function () { return H.location('href'); };
    location.ancestorOrigins = [];

    // `location = '...'` is the shortest way to navigate and pages use it, so
    // the global needs a setter of its own and not just the object.
    Object.defineProperty(global, 'location', {
        configurable: true,
        get: function () { return location; },
        set: function (value) { H.navigate(String(value)); },
    });

    global.history = {
        length: 1,
        state: null,
        scrollRestoration: 'auto',
        back: function () {},
        forward: function () {},
        go: function () {},
        // The address bar is not ours to rewrite from here, and pretending
        // otherwise would leave it disagreeing with the page.
        pushState: function () {},
        replaceState: function () {},
    };

    // ── storage ─────────────────────────────────────────────────────────────

    function makeStorage(kind) {
        var methods = {
            getItem: function (key) { return H.storage(kind, 'get', String(key), ''); },
            setItem: function (key, value) { H.storage(kind, 'set', String(key), String(value)); },
            removeItem: function (key) { H.storage(kind, 'remove', String(key), ''); },
            clear: function () { H.storage(kind, 'clear', '', ''); },
            key: function (index) { return H.storage(kind, 'key', String(+index), ''); },
        };
        // A Proxy so that `localStorage.token = 'x'` works as well as
        // `setItem`, which is how about half of the pages that use it are
        // written.
        return new Proxy(methods, {
            get: function (target, key) {
                if (typeof key !== 'string') return target[key];
                if (key === 'length') return +H.storage(kind, 'length', '', '');
                if (key in target) return target[key];
                var value = H.storage(kind, 'get', key, '');
                return value === null ? undefined : value;
            },
            set: function (target, key, value) {
                if (typeof key !== 'string') return false;
                H.storage(kind, 'set', key, String(value));
                return true;
            },
            deleteProperty: function (target, key) {
                H.storage(kind, 'remove', String(key), '');
                return true;
            },
            has: function (target, key) {
                return typeof key === 'string'
                    && (key in target || H.storage(kind, 'get', key, '') !== null);
            },
        });
    }

    global.localStorage = makeStorage('local');
    global.sessionStorage = makeStorage('session');

    // ── the network, which is not here ──────────────────────────────────────

    // A rejected promise rather than a missing function: a page that guards its
    // fetch with `.catch` takes its own error path, which is far better than a
    // TypeError stopping the script that was going to render the rest of the
    // page.
    global.fetch = function (url) {
        return Promise.reject(new TypeError(
            'fetch is not implemented in this browser (' + String(url) + ')'));
    };
    global.XMLHttpRequest = function () {
        this.readyState = 0;
        this.status = 0;
        this.responseText = '';
        this.onload = null;
        this.onerror = null;
        this.onreadystatechange = null;
        this.open = function () { this.readyState = 1; };
        this.setRequestHeader = function () {};
        this.abort = function () {};
        this.getAllResponseHeaders = function () { return ''; };
        this.send = function () {
            var self = this;
            setTimeout(function () {
                self.readyState = 4;
                if (self.onreadystatechange) self.onreadystatechange();
                if (self.onerror) self.onerror(new DOMEvent('error'));
            }, 0);
        };
    };

    // Constructors a page may reach for. `Node`, `Element` and `HTMLElement` are
    // all the one wrapper class here, so `instanceof` says yes a little more
    // often than a real browser would.
    global.Node = Node;
    global.Element = Node;
    global.HTMLElement = Node;
    global.HTMLInputElement = Node;
    global.Text = Node;
    global.Event = DOMEvent;
    global.CustomEvent = DOMEvent;
    global.MouseEvent = DOMEvent;
    global.KeyboardEvent = DOMEvent;
    global.DOMTokenList = DOMTokenList;
    global.NodeList = Array;
    global.HTMLCollection = Array;
    global.MutationObserver = function () {
        this.observe = function () {};
        this.disconnect = function () {};
        this.takeRecords = function () { return []; };
    };
    global.IntersectionObserver = global.MutationObserver;
    global.ResizeObserver = global.MutationObserver;

    // ── the embedder's own entry points ─────────────────────────────────────

    /// Evaluate a page's script. Kept here rather than on the Rust side so that
    /// `setTimeout('code')` and an `on...` attribute reach the same compiler.
    function evaluate(source) {
        return new Function(source).call(global);
    }

    // Named so that nothing a page is likely to define can collide, and
    // non-enumerable so that a page walking the global object does not find
    // them. Rust calls these by name through `Engine::call_global`.
    Object.defineProperties(global, {
        __os101_dispatch: {
            value: function (id, type, initJson) {
                ran = 0;
                var init;
                try {
                    init = initJson ? JSON.parse(initJson) : {};
                } catch (e) {
                    init = {};
                }
                init.isTrusted = true;
                var event = new DOMEvent(type, init);
                dispatchOn(+id, event);
                // Bit 0: something handled it. Bit 1: it asked us not to do
                // whatever we were going to do.
                return (ran > 0 ? 1 : 0) | (event.defaultPrevented ? 2 : 0);
            },
        },
        // One call both does the work and says whether to come back for more, so
        // the browser's idle path does not need a second one to find out.
        __os101_tick: {
            value: function (now) {
                var count = runDue(now);
                return count + ',' + (pending() ? 1 : 0);
            },
        },
        __os101_pending: { value: pending },
        __os101_evaluate: { value: evaluate },
        __os101_describe: { value: function (value) { return show(value, 0); } },
    });

    // Nothing below this line: the natives are taken off the global object so a
    // page can neither call them nor replace them, and everything above holds
    // its own reference.
    for (var n = 0; n < NATIVES.length; n++) {
        delete global[NATIVES[n]];
    }
})(globalThis);
