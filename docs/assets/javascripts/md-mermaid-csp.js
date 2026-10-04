// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/*
 * PROJECT CODE (not vendored) — CSP shim for Material's mermaid integration.
 *
 * Vendoring mermaid same-origin (assets/javascripts/mermaid-11.16.1.min.js,
 * wired via extra_javascript) is what lets the library load at all under
 * production's `script-src 'self'`. It is not sufficient on its own, because
 * the same policy also sends `style-src-elem 'self'` and mermaid delivers a
 * diagram's entire stylesheet — node fills, edge strokes, label fonts, ~11KB
 * of it — as a <style> ELEMENT. Measured, that costs two things, not one:
 *
 *   1. mermaid puts that <style> into the live document while it draws, so the
 *      browser refuses it and logs a CSP error on every diagram page; and
 *   2. because it is refused, labels are measured in the wrong font and the
 *      diagram comes out mis-sized — "backend/crates" clipped to "backend/(",
 *      node text overflowing its own box.
 *
 * Material then copies the same markup into a CLOSED shadow root, where a
 * further refusal leaves the diagram unpainted. Page CSS cannot repair any of
 * this: docs/assets/netroll.css can set the --md-mermaid-* custom properties,
 * which inherit across a shadow boundary, but it cannot reach rules inside one.
 * Emptying the element does not help either — an empty <style> is refused too
 * (its hash is the hash of the empty string), so it has to stay out of the
 * document entirely.
 *
 * CSP governs style ELEMENTS, not the CSSOM: a constructed stylesheet carries
 * the identical rules and `style-src-elem` does not apply to it. So this shim
 * intercepts mermaid's stylesheet at the moment it would be inserted, keeps it
 * out of the DOM, and re-homes the rules in adoptedStyleSheets — on the
 * document while mermaid measures and draws, then on the shadow root once
 * Material hands it over. The rules are id-scoped (`#__mermaid_0 …`) and the
 * document-level sheet is dropped as soon as the render resolves, so nothing
 * leaks into the rest of the page.
 *
 * The theme keeps ownership of its own integration. Material's bundle does
 * `let {svg, fn} = await mermaid.render(...); shadow.innerHTML = svg; fn?.(shadow)`
 * — it calls back with the shadow root if render returned a bind function.
 * mermaid 11 names that key `bindFunctions`, so Material's `fn` is vestigial
 * and always undefined; both names are populated here so the shim survives the
 * theme catching up. Nothing below initialises mermaid, observes the DOM, or
 * decides when to draw: that all stays Material's, so there is no second
 * pipeline to race the first.
 *
 * The DOM patch is deliberately narrow. It is installed only while a render is
 * in flight, and it diverts a node only when that node is a <style> whose text
 * names the id of a render currently in flight. Everything else — including
 * every other node d3 inserts while drawing — goes straight to the native
 * method.
 *
 * ON A MATERIAL OR MERMAID UPGRADE: re-check that the theme still consumes a
 * post-render callback and that mermaid still ships its CSS in a <style>
 * element. If either stops being true this file becomes dead weight — it fails
 * safe (see the capability guard), but it should be deleted rather than left.
 */
(function () {
    // Every bail-out and pass-through below degrades to exactly the state this
    // file exists to prevent, and does so only under the production CSP — never
    // in local preview. So each one says so out loud: a warning in the console
    // is the only signal a reader or a reviewer can ever get.
    var TAG = 'md-mermaid-csp.js: ';

    var mermaidLib = window.mermaid;
    if (!mermaidLib || typeof mermaidLib.render !== 'function') {
        console.warn(TAG + 'window.mermaid is missing or has no render(); the ' +
            'vendored bundle did not load. Diagrams will fall back to the ' +
            'theme\'s CSP-refused unpkg.com fetch and will not render.');
        return;
    }

    // Without constructed stylesheets there is nowhere CSP-safe to put the
    // rules. Bow out entirely rather than half-apply: a diagram styled by a
    // refused <style> still beats no diagram at all.
    var supported =
        typeof CSSStyleSheet === 'function' &&
        typeof CSSStyleSheet.prototype.replaceSync === 'function' &&
        'adoptedStyleSheets' in Document.prototype &&
        'adoptedStyleSheets' in ShadowRoot.prototype;
    if (!supported) {
        console.warn(TAG + 'constructed stylesheets are unsupported here; ' +
            'mermaid\'s <style> is left to the browser, so style-src-elem will ' +
            'refuse it and diagrams will be mis-sized or unpainted.');
        return;
    }

    var nativeInsertBefore = Node.prototype.insertBefore;
    var nativeAppendChild = Node.prototype.appendChild;
    var originalRender = mermaidLib.render.bind(mermaidLib);
    var STYLE_TAG = /<style[^>]*>([\s\S]*?)<\/style>/gi;

    var inFlight = [];

    // `#__mermaid_1` is a prefix of `#__mermaid_10`, so a bare substring test
    // hands entry 1 the stylesheet belonging to entry 10 whenever both are in
    // flight. Require a CSS-identifier boundary after the id instead.
    function markerFor(id) {
        var escaped = String(id).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
        return new RegExp('#' + escaped + '(?![A-Za-z0-9_-])');
    }

    function claim(css) {
        if (typeof css !== 'string' || !css) {
            return null;
        }
        for (var i = 0; i < inFlight.length; i++) {
            if (inFlight[i].marker.test(css)) {
                return inFlight[i];
            }
        }
        return null;
    }

    function absorb(entry, css) {
        entry.css += css;
        entry.sheet.replaceSync(entry.css);
        if (document.adoptedStyleSheets.indexOf(entry.sheet) === -1) {
            document.adoptedStyleSheets = document.adoptedStyleSheets.concat(entry.sheet);
        }
    }

    /** Truthy when this node is a mermaid stylesheet we have taken over. */
    function diverted(node) {
        if (!node || node.nodeType !== 1 || node.tagName !== 'STYLE') {
            return false;
        }
        var entry = claim(node.textContent);
        if (!entry) {
            // A <style> that names no in-flight render is left to the browser,
            // where style-src-elem refuses it. Known case: the cytoscape-backed
            // diagram types (mindmap, architecture) inject a container
            // stylesheet that carries no `#__mermaid_N` selector at all.
            console.warn(TAG + 'a <style> inserted during a mermaid render was ' +
                'not claimed by any in-flight diagram; style-src-elem will ' +
                'refuse it. First 80 chars: ' +
                String(node.textContent || '').slice(0, 80));
            return false;
        }
        absorb(entry, node.textContent);
        return true;
    }

    function enter(entry) {
        inFlight.push(entry);
        if (inFlight.length > 1) {
            return;
        }
        Node.prototype.insertBefore = function (node, reference) {
            return diverted(node) ? node : nativeInsertBefore.call(this, node, reference);
        };
        Node.prototype.appendChild = function (node) {
            return diverted(node) ? node : nativeAppendChild.call(this, node);
        };
    }

    function leave(entry) {
        var at = inFlight.indexOf(entry);
        if (at !== -1) {
            inFlight.splice(at, 1);
        }
        if (inFlight.length === 0) {
            Node.prototype.insertBefore = nativeInsertBefore;
            Node.prototype.appendChild = nativeAppendChild;
        }
        document.adoptedStyleSheets = document.adoptedStyleSheets.filter(function (sheet) {
            return sheet !== entry.sheet;
        });
    }

    mermaidLib.render = function (id, text, container) {
        var entry = { marker: markerFor(id), css: '', sheet: new CSSStyleSheet() };
        enter(entry);

        var settled;
        try {
            settled = Promise.resolve(originalRender(id, text, container));
        } catch (error) {
            leave(entry);
            throw error;
        }

        return settled.then(
            function (result) {
                // `.then(onFulfilled, onRejected)` cannot catch its own
                // fulfilment handler, so anything thrown below — a mermaid
                // upgrade changing the result shape, say — would strand this
                // entry in inFlight and leave Node.prototype patched for the
                // life of the page. finally is what makes leave() unmissable.
                try {
                    // Fold in any stylesheet a future mermaid delivers some
                    // other way, and drop the tags, so nothing style-shaped
                    // reaches the shadow root for CSP to refuse there instead.
                    var svg = String(result.svg).replace(STYLE_TAG, function (_, body) {
                        if (body) {
                            entry.css += body;
                        }
                        return '';
                    });
                    var sheet = entry.sheet;
                    var css = entry.css;
                    if (!css) {
                        console.warn(TAG + 'mermaid render "' + id + '" produced ' +
                            'no stylesheet to re-home. The diagram will draw ' +
                            'unstyled unless the theme supplies its own CSS.');
                        // Still return the STRIPPED svg: an empty <style> is
                        // refused under the hash of the empty string, so it has
                        // to stay out of the document just like a full one.
                        return Object.assign({}, result, { svg: svg });
                    }
                    sheet.replaceSync(css);

                    var bind = result.bindFunctions;
                    var adopt = function (root) {
                        if (root && root.adoptedStyleSheets.indexOf(sheet) === -1) {
                            root.adoptedStyleSheets = root.adoptedStyleSheets.concat(sheet);
                        }
                        if (typeof bind === 'function') {
                            bind(root);
                        }
                    };

                    return Object.assign({}, result, {
                        svg: svg,
                        fn: adopt,
                        bindFunctions: adopt
                    });
                } finally {
                    leave(entry);
                }
            },
            function (error) {
                leave(entry);
                throw error;
            }
        );
    };
})();
