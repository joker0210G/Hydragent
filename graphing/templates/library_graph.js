/* ──────────────────────────────────────────────────────────────────────────── */
/* Hydragent Library Graph — library_graph.js                                  */
/* Uses D3 v7. graphData is injected by the Python builder.                    */
/* ──────────────────────────────────────────────────────────────────────────── */

(function () {
    "use strict";

    /* ── Colours / sizes shared with CSS ─────────────────────────────────── */
    const COLOR = { shelf: "#a855f7", book: "#3b82f6", page: "#10b981", tag: "#f59e0b" };
    const RADIUS = { shelf: 16, book: 11, page: 7 };
    const LABEL_MAX = { shelf: 28, book: 22, page: 18 };

    /* ── Guard: empty graph ───────────────────────────────────────────────── */
    const loader = document.getElementById("loader");
    const emptyState = document.getElementById("empty-state");

    if (!graphData || !graphData.nodes || graphData.nodes.length === 0) {
        loader.classList.add("hidden");
        emptyState.hidden = false;
        return;
    }

    /* ── Dimensions ───────────────────────────────────────────────────────── */
    let W = window.innerWidth;
    let H = window.innerHeight;

    /* ── State ───────────────────────────────────────────────────────────── */
    let selectedNode = null;
    let hiddenTypes  = new Set();   // node types currently filtered out
    let searchQuery  = "";

    /* ── Build index structures ───────────────────────────────────────────── */
    // Node map for fast lookup
    const nodeById = Object.fromEntries(graphData.nodes.map(n => [n.id, n]));

    // Adjacency list: nodeId → [{node, edge}]
    const adjacency = {};
    graphData.nodes.forEach(n => { adjacency[n.id] = []; });
    graphData.links.forEach(e => {
        const s = typeof e.source === "object" ? e.source.id : e.source;
        const t = typeof e.target === "object" ? e.target.id : e.target;
        if (adjacency[s]) adjacency[s].push({ node: nodeById[t], edge: e });
        if (adjacency[t]) adjacency[t].push({ node: nodeById[s], edge: e });
    });

    /* ── Stats strip ─────────────────────────────────────────────────────── */
    const typeCounts = { shelf: 0, book: 0, page: 0 };
    graphData.nodes.forEach(n => { if (n.type in typeCounts) typeCounts[n.type]++; });
    document.getElementById("stat-shelves").textContent = typeCounts.shelf;
    document.getElementById("stat-books").textContent   = typeCounts.book;
    document.getElementById("stat-pages").textContent   = typeCounts.page;

    /* ── SVG setup ───────────────────────────────────────────────────────── */
    const svg = d3.select("#graph-container")
        .append("svg")
        .attr("width", "100%")
        .attr("height", "100%")
        .attr("viewBox", [0, 0, W, H]);

    const defs = svg.append("defs");

    // Arrow marker (unused for now — kept for future directed edges)
    defs.append("marker")
        .attr("id", "arrow")
        .attr("viewBox", "0 -5 10 10").attr("refX", 18).attr("refY", 0)
        .attr("markerWidth", 6).attr("markerHeight", 6).attr("orient", "auto")
        .append("path").attr("d", "M0,-5L10,0L0,5").attr("fill", "#5b547a");

    // Radial gradient for glow effect behind selected nodes
    const grd = defs.append("radialGradient").attr("id", "glow-gradient")
        .attr("cx", "50%").attr("cy", "50%").attr("r", "50%");
    grd.append("stop").attr("offset", "0%").attr("stop-color", "#a855f7").attr("stop-opacity", 0.25);
    grd.append("stop").attr("offset", "100%").attr("stop-color", "#a855f7").attr("stop-opacity", 0);

    const g = svg.append("g");   // main transform group

    /* ── Zoom behaviour ───────────────────────────────────────────────────── */
    const zoom = d3.zoom()
        .scaleExtent([0.05, 10])
        .on("zoom", (event) => {
            g.attr("transform", event.transform);
            updateMinimap(event.transform);
        });
    svg.call(zoom).on("dblclick.zoom", null);

    /* ── Force simulation ─────────────────────────────────────────────────── */
    const simulation = d3.forceSimulation(graphData.nodes)
        .force("link", d3.forceLink(graphData.links)
            .id(d => d.id)
            .distance(d => {
                if (d.relation === "belongs_to") return 55;
                if (d.relation === "sits_on")    return 130;
                return 80;
            })
            .strength(d => d.relation === "belongs_to" ? 0.9 : 0.6)
        )
        .force("charge", d3.forceManyBody()
            .strength(d => {
                if (d.type === "shelf") return -600;
                if (d.type === "book")  return -280;
                return -120;
            })
            .distanceMax(600)
        )
        .force("center", d3.forceCenter(W / 2, H / 2).strength(0.05))
        .force("collision", d3.forceCollide().radius(d => (RADIUS[d.type] || 7) + 16).strength(0.8))
        .force("x", d3.forceX(W / 2).strength(0.02))
        .force("y", d3.forceY(H / 2).strength(0.02))
        .alphaDecay(0.025)
        .velocityDecay(0.4);

    /* ── Link layer ───────────────────────────────────────────────────────── */
    const linkGroup = g.append("g").attr("class", "links");
    let link = linkGroup.selectAll("line")
        .data(graphData.links)
        .join("line")
        .attr("class", d => `link ${d.relation}`)
        .attr("stroke-width", d => d.relation === "belongs_to" ? 2 : 1.2);

    /* ── Node layer ───────────────────────────────────────────────────────── */
    const nodeGroup = g.append("g").attr("class", "nodes");
    let node = nodeGroup.selectAll("circle")
        .data(graphData.nodes)
        .join("circle")
        .attr("class", "node")
        .attr("tabindex", "0")
        .attr("r", d => RADIUS[d.type] || 7)
        .attr("fill", d => COLOR[d.type] || COLOR.page)
        .attr("stroke", "#08070e")
        .attr("stroke-width", 1.5)
        .attr("aria-label", d => `${d.type}: ${d.label}`)
        .call(dragBehaviour());

    /* ── Label layer ──────────────────────────────────────────────────────── */
    const labelGroup = g.append("g").attr("class", "labels");
    let label = labelGroup.selectAll("text")
        .data(graphData.nodes)
        .join("text")
        .attr("class", d => `node-label ${d.type}-label`)
        .attr("dy", d => -(RADIUS[d.type] || 7) - 5)
        .attr("text-anchor", "middle")
        .classed("hidden", d => d.type === "page")   // hide page labels by default — too many
        .text(d => truncate(d.label, LABEL_MAX[d.type] || 18));

    /* ── Simulation tick ─────────────────────────────────────────────────── */
    simulation.on("tick", () => {
        link
            .attr("x1", d => d.source.x).attr("y1", d => d.source.y)
            .attr("x2", d => d.target.x).attr("y2", d => d.target.y);
        node.attr("cx", d => d.x).attr("cy", d => d.y);
        label.attr("x", d => d.x).attr("y", d => d.y);
    });

    // After settling, draw minimap once and fit view
    simulation.on("end", () => {
        drawMinimapNodes();
        fitGraph();
    });

    /* ── Tooltip ──────────────────────────────────────────────────────────── */
    const tooltip = document.getElementById("tooltip");

    node.on("mouseenter", (event, d) => {
        const props = d.properties || {};
        let metaHtml = "";
        if (d.type === "shelf")      metaHtml = `${props.book_count || "?"} books`;
        else if (d.type === "book")  metaHtml = `${props.page_count || "?"} pages`;
        else if (d.type === "page")  metaHtml = adjacency[d.id].length + " connection(s)";

        tooltip.innerHTML = `
            <div class="tt-type ${d.type}">${d.type}</div>
            <div class="tt-label">${escHtml(d.label)}</div>
            ${metaHtml ? `<div class="tt-meta">${escHtml(metaHtml)}</div>` : ""}
        `;
        tooltip.setAttribute("aria-hidden", "false");
        positionTooltip(event);
        tooltip.classList.add("visible");
    })
    .on("mousemove", positionTooltip)
    .on("mouseleave", () => {
        tooltip.classList.remove("visible");
        tooltip.setAttribute("aria-hidden", "true");
    });

    function positionTooltip(event) {
        const pad = 14;
        const tt = tooltip.getBoundingClientRect();
        let x = event.clientX + pad;
        let y = event.clientY + pad;
        if (x + tt.width  > window.innerWidth)  x = event.clientX - tt.width  - pad;
        if (y + tt.height > window.innerHeight) y = event.clientY - tt.height - pad;
        tooltip.style.left = x + "px";
        tooltip.style.top  = y + "px";
    }

    /* ── Click / keyboard selection ──────────────────────────────────────── */
    node.on("click", (event, d) => { event.stopPropagation(); selectNode(d); });
    node.on("keydown", (event, d) => {
        if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            selectNode(d);
        }
    });
    svg.on("click", clearSelection);

    function selectNode(d) {
        selectedNode = d;

        // Connected node ids
        const connected = new Set([d.id]);
        graphData.links.forEach(l => {
            const s = l.source.id, t = l.target.id;
            if (s === d.id) connected.add(t);
            if (t === d.id) connected.add(s);
        });

        node.classed("active-focus", n => n.id === d.id)
            .classed("faded", n => !connected.has(n.id));
        label.classed("hidden", n => {
            if (n.type !== "page") return false;
            return !connected.has(n.id);
        });
        link.classed("active", l => l.source.id === d.id || l.target.id === d.id)
            .classed("faded", l => !(l.source.id === d.id || l.target.id === d.id));

        populateSidebar(d, connected);
        panToNode(d);
    }

    function clearSelection() {
        selectedNode = null;
        node.classed("active-focus faded", false);
        label.classed("hidden", d => d.type === "page");
        link.classed("active faded", false);
        applyHiddenTypes();     // re-apply filter visibility
        renderEmptyDetails();
    }

    /* ── Sidebar details ─────────────────────────────────────────────────── */
    function populateSidebar(d, connected) {
        const panel = document.getElementById("details-panel");
        const props = d.properties || {};

        // Connections list (excluding self)
        const conns = (adjacency[d.id] || [])
            .filter(c => c.node)
            .map(c => ({ node: c.node, rel: c.edge.relation }));

        let connHtml = "";
        if (conns.length) {
            const items = conns.map(c => {
                const col = COLOR[c.node.type] || COLOR.page;
                const lbl = truncate(c.node.label, 30);
                const relLabel = c.rel === "belongs_to" ? "↑ in"
                               : c.rel === "sits_on"    ? "↑ on"
                               : c.rel;
                return `<li class="conn-item" role="button" tabindex="0"
                            data-nid="${escAttr(c.node.id)}"
                            aria-label="Navigate to ${escAttr(c.node.label)}">
                    <span class="conn-dot" style="background:${col};"></span>
                    <span class="conn-label">${escHtml(lbl)}</span>
                    <span class="conn-rel">${escHtml(relLabel)}</span>
                </li>`;
            }).join("");
            connHtml = `
                <div class="detail-card">
                    <div class="detail-card-title">Connections (${conns.length})</div>
                    <div class="detail-card-body" style="padding:8px;">
                        <ul class="conn-list">${items}</ul>
                    </div>
                </div>`;
        }

        // Properties card
        let metaRows = `<div class="meta-row"><span class="meta-label">Type</span><span class="meta-value">${escHtml(d.type)}</span></div>`;
        if (d.type === "shelf" && props.book_count != null)
            metaRows += `<div class="meta-row"><span class="meta-label">Books</span><span class="meta-value">${props.book_count}</span></div>`;
        if (d.type === "book" && props.page_count != null)
            metaRows += `<div class="meta-row"><span class="meta-label">Pages</span><span class="meta-value">${props.page_count}</span></div>`;
        metaRows += `<div class="meta-row"><span class="meta-label">Connections</span><span class="meta-value">${conns.length}</span></div>`;

        // Summary
        const summary = props.summary || props.description || null;
        const summaryHtml = summary ? `
            <div class="detail-card">
                <div class="detail-card-title">Summary</div>
                <div class="detail-card-body"><p class="summary-text">${escHtml(summary)}</p></div>
            </div>` : "";

        // Tags (for page nodes)
        const tags = props.tags || props.suggested_books || [];
        const tagHtml = tags.length ? `
            <div class="detail-card">
                <div class="detail-card-title">Tags</div>
                <div class="detail-card-body">
                    <div class="tag-cloud">
                        ${tags.slice(0, 12).map(t => `<span class="tag-pill">${escHtml(t)}</span>`).join("")}
                    </div>
                </div>
            </div>` : "";

        panel.innerHTML = `
            <div class="node-details">
                <div class="detail-header">
                    <span class="detail-badge badge-${escAttr(d.type)}">${escHtml(d.type)}</span>
                    <h2 class="detail-title">${escHtml(d.label)}</h2>
                </div>
                <div class="detail-card">
                    <div class="detail-card-title">Properties</div>
                    <div class="detail-card-body">${metaRows}</div>
                </div>
                ${summaryHtml}
                ${tagHtml}
                ${connHtml}
            </div>`;

        // Attach click handlers to connection items
        panel.querySelectorAll(".conn-item").forEach(el => {
            const nid = el.dataset.nid;
            const target = nodeById[nid];
            if (!target) return;
            const go = () => selectNode(target);
            el.addEventListener("click", go);
            el.addEventListener("keydown", e => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); go(); } });
        });
    }

    function renderEmptyDetails() {
        document.getElementById("details-panel").innerHTML = `
            <div class="empty-details">
                <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5">
                    <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>
                </svg>
                Click any node to explore its connections and properties.
            </div>`;
    }

    /* ── Panning to a node ───────────────────────────────────────────────── */
    function panToNode(d) {
        const t = d3.zoomTransform(svg.node());
        const k = Math.max(t.k, 0.8);
        const tx = W / 2 - d.x * k;
        const ty = H / 2 - d.y * k;
        svg.transition().duration(600).ease(d3.easeCubicOut)
            .call(zoom.transform, d3.zoomIdentity.translate(tx, ty).scale(k));
    }

    /* ── Drag behaviour ──────────────────────────────────────────────────── */
    function dragBehaviour() {
        return d3.drag()
            .on("start", (event, d) => {
                if (!event.active) simulation.alphaTarget(0.25).restart();
                d.fx = d.x; d.fy = d.y;
            })
            .on("drag", (event, d) => { d.fx = event.x; d.fy = event.y; })
            .on("end", (event, d) => {
                if (!event.active) simulation.alphaTarget(0);
                d.fx = null; d.fy = null;
            });
    }

    /* ── Filter buttons ──────────────────────────────────────────────────── */
    document.querySelectorAll(".filter-btn").forEach(btn => {
        btn.addEventListener("click", () => {
            const type = btn.dataset.type;
            const isActive = btn.classList.toggle("active");
            btn.setAttribute("aria-pressed", isActive ? "true" : "false");

            if (isActive) hiddenTypes.delete(type);
            else          hiddenTypes.add(type);

            applyHiddenTypes();
            if (selectedNode && hiddenTypes.has(selectedNode.type)) clearSelection();
        });
    });

    function applyHiddenTypes() {
        node.style("display", d => hiddenTypes.has(d.type) ? "none" : null);
        label.style("display", d => hiddenTypes.has(d.type) ? "none" : null);
        // Hide links where either endpoint is hidden
        link.style("display", d => {
            const sType = (d.source.type || d.source);
            const tType = (d.target.type || d.target);
            if (hiddenTypes.has(sType) || hiddenTypes.has(tType)) return "none";
            return null;
        });
    }

    /* ── Search ──────────────────────────────────────────────────────────── */
    document.getElementById("search-input").addEventListener("input", function () {
        searchQuery = this.value.toLowerCase().trim();
        if (!searchQuery) {
            clearSelection();
            return;
        }

        let best = null, bestScore = -1;
        graphData.nodes.forEach(n => {
            if (hiddenTypes.has(n.type)) return;
            const lbl = n.label.toLowerCase();
            if (lbl === searchQuery) { if (bestScore < 3) { best = n; bestScore = 3; } }
            else if (lbl.startsWith(searchQuery)) { if (bestScore < 2) { best = n; bestScore = 2; } }
            else if (lbl.includes(searchQuery)) { if (bestScore < 1) { best = n; bestScore = 1; } }
        });

        // Dim non-matching
        node.style("opacity", n => {
            const match = n.label.toLowerCase().includes(searchQuery);
            return (match && !hiddenTypes.has(n.type)) ? 1 : 0.1;
        });
        label.style("opacity", n => n.label.toLowerCase().includes(searchQuery) ? 1 : 0.05);
        link.style("opacity", 0.08);

        if (best) panToNode(best);
    });

    /* ── Buttons: Fit & Reset ────────────────────────────────────────────── */
    document.getElementById("btn-fit").addEventListener("click", fitGraph);
    document.getElementById("btn-reset").addEventListener("click", () => {
        svg.transition().duration(600).ease(d3.easeCubicOut)
            .call(zoom.transform, d3.zoomIdentity);
    });

    function fitGraph() {
        const bbox = nodeGroup.node().getBBox();
        if (!bbox.width || !bbox.height) return;
        const pad = 60;
        const scaleX = (W - pad * 2) / bbox.width;
        const scaleY = (H - pad * 2) / bbox.height;
        const k = Math.min(scaleX, scaleY, 2);
        const tx = W / 2 - k * (bbox.x + bbox.width  / 2);
        const ty = H / 2 - k * (bbox.y + bbox.height / 2);
        svg.transition().duration(700).ease(d3.easeCubicOut)
            .call(zoom.transform, d3.zoomIdentity.translate(tx, ty).scale(k));
    }

    /* ── Sidebar toggle ──────────────────────────────────────────────────── */
    const sidebarEl  = document.getElementById("sidebar");
    const toggleBtn  = document.getElementById("toggle-sidebar");
    toggleBtn.addEventListener("click", () => {
        const collapsed = sidebarEl.classList.toggle("collapsed");
        toggleBtn.classList.toggle("collapsed", collapsed);
        toggleBtn.setAttribute("aria-expanded", collapsed ? "false" : "true");
    });

    /* ── Mini-map ─────────────────────────────────────────────────────────── */
    const minimapCanvas  = document.getElementById("minimap");
    const mmCtx          = minimapCanvas.getContext("2d");
    const MM_W = minimapCanvas.width;
    const MM_H = minimapCanvas.height;

    let minimapScale = 1, minimapOffsetX = 0, minimapOffsetY = 0;

    function drawMinimapNodes() {
        if (!graphData.nodes.length) return;

        const xs = graphData.nodes.map(n => n.x || 0);
        const ys = graphData.nodes.map(n => n.y || 0);
        const minX = Math.min(...xs), maxX = Math.max(...xs);
        const minY = Math.min(...ys), maxY = Math.max(...ys);
        const gW = maxX - minX || 1, gH = maxY - minY || 1;
        const pad = 8;

        minimapScale   = Math.min((MM_W - pad * 2) / gW, (MM_H - pad * 2) / gH);
        minimapOffsetX = pad - minX * minimapScale;
        minimapOffsetY = pad - minY * minimapScale;

        mmCtx.clearRect(0, 0, MM_W, MM_H);

        // Edges
        mmCtx.lineWidth = 0.6;
        graphData.links.forEach(l => {
            const sx = l.source.x * minimapScale + minimapOffsetX;
            const sy = l.source.y * minimapScale + minimapOffsetY;
            const tx = l.target.x * minimapScale + minimapOffsetX;
            const ty = l.target.y * minimapScale + minimapOffsetY;
            mmCtx.strokeStyle = l.relation === "belongs_to" ? "rgba(59,130,246,.4)" : "rgba(168,85,247,.4)";
            mmCtx.beginPath(); mmCtx.moveTo(sx, sy); mmCtx.lineTo(tx, ty); mmCtx.stroke();
        });

        // Nodes
        graphData.nodes.forEach(n => {
            const x  = (n.x || 0) * minimapScale + minimapOffsetX;
            const y  = (n.y || 0) * minimapScale + minimapOffsetY;
            const r  = Math.max(1.5, (RADIUS[n.type] || 5) * minimapScale * 1.2);
            mmCtx.beginPath();
            mmCtx.arc(x, y, r, 0, Math.PI * 2);
            mmCtx.fillStyle = COLOR[n.type] || COLOR.page;
            mmCtx.globalAlpha = 0.85;
            mmCtx.fill();
            mmCtx.globalAlpha = 1;
        });
    }

    const mmViewport = document.getElementById("minimap-viewport");

    function updateMinimap(transform) {
        drawMinimapNodes();

        // Viewport indicator
        const vpX = (-transform.x / transform.k) * minimapScale + minimapOffsetX;
        const vpY = (-transform.y / transform.k) * minimapScale + minimapOffsetY;
        const vpW = (W / transform.k) * minimapScale;
        const vpH = (H / transform.k) * minimapScale;

        // Position element relative to minimap canvas
        const canvasRect = minimapCanvas.getBoundingClientRect();
        mmViewport.style.left  = (canvasRect.left + vpX) + "px";
        mmViewport.style.top   = (canvasRect.top  + vpY) + "px";
        mmViewport.style.width  = vpW + "px";
        mmViewport.style.height = vpH + "px";
    }

    // Click on minimap to pan
    minimapCanvas.addEventListener("click", (e) => {
        const rect = minimapCanvas.getBoundingClientRect();
        const mx = e.clientX - rect.left;
        const my = e.clientY - rect.top;
        const graphX = (mx - minimapOffsetX) / minimapScale;
        const graphY = (my - minimapOffsetY) / minimapScale;
        const t = d3.zoomTransform(svg.node());
        svg.transition().duration(400).call(
            zoom.transform,
            d3.zoomIdentity.translate(W / 2 - graphX * t.k, H / 2 - graphY * t.k).scale(t.k)
        );
    });

    /* ── Window resize ───────────────────────────────────────────────────── */
    window.addEventListener("resize", () => {
        W = window.innerWidth;
        H = window.innerHeight;
        svg.attr("viewBox", [0, 0, W, H]);
        simulation.force("center", d3.forceCenter(W / 2, H / 2));
        simulation.force("x", d3.forceX(W / 2).strength(0.02));
        simulation.force("y", d3.forceY(H / 2).strength(0.02));
        simulation.alpha(0.2).restart();
    });

    /* ── Helpers ─────────────────────────────────────────────────────────── */
    function truncate(str, max) {
        if (!str) return "";
        return str.length > max ? str.substring(0, max) + "…" : str;
    }

    function escHtml(str) {
        return String(str)
            .replace(/&/g, "&amp;").replace(/</g, "&lt;")
            .replace(/>/g, "&gt;").replace(/"/g, "&quot;");
    }

    function escAttr(str) { return escHtml(str).replace(/'/g, "&#39;"); }

    /* ── Dismiss loader when simulation starts ───────────────────────────── */
    setTimeout(() => loader.classList.add("hidden"), 600);

})();
