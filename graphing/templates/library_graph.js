// Fade out loader on window load
window.addEventListener('load', () => {
    const loader = document.getElementById('loader');
    loader.style.opacity = '0';
    setTimeout(() => loader.style.display = 'none', 500);
});

const width = window.innerWidth;
const height = window.innerHeight;

const colors = {
    shelf: "#a855f7",
    book: "#3b82f6",
    page: "#10b981",
    tag: "#f59e0b"
};

const radius = {
    shelf: 14,
    book: 10,
    page: 7,
    tag: 5
};

const processedNodes = [...graphData.nodes];
const processedLinks = [...graphData.links];

const svg = d3.select("#graph-container")
    .append("svg")
    .attr("width", "100%")
    .attr("height", "100%")
    .attr("viewBox", [0, 0, width, height]);
    
const g = svg.append("g");

const zoom = d3.zoom()
    .scaleExtent([0.1, 8])
    .on("zoom", (event) => {
        g.attr("transform", event.transform);
    });
    
svg.call(zoom);

const simulation = d3.forceSimulation(processedNodes)
    .force("link", d3.forceLink(processedLinks).id(d => d.id).distance(d => {
        if (d.relation === 'belongs_to') return 40;
        if (d.relation === 'sits_on') return 100;
        return 60;
    }))
    .force("charge", d3.forceManyBody().strength(-200))
    .force("center", d3.forceCenter(width / 2, height / 2))
    .force("collision", d3.forceCollide().radius(d => (radius[d.type] || 7) + 12));

const link = g.append("g")
    .selectAll("line")
    .data(processedLinks)
    .join("line")
    .attr("class", d => `link ${d.relation}`)
    .attr("stroke-width", d => d.relation === 'belongs_to' ? 2 : 1);

const node = g.append("g")
    .selectAll("circle")
    .data(processedNodes)
    .join("circle")
    .attr("class", "node")
    .attr("tabindex", "0")
    .attr("r", d => radius[d.type] || 7)
    .attr("fill", d => colors[d.type] || "#10b981")
    .attr("stroke", "#08070e")
    .attr("stroke-width", 1.5)
    .call(drag(simulation));
    
const label = g.append("g")
    .selectAll("text")
    .data(processedNodes)
    .join("text")
    .attr("class", "node-label")
    .attr("dy", d => -(radius[d.type] || 7) - 4)
    .attr("text-anchor", "middle")
    .text(d => d.label.length > 20 ? d.label.substring(0, 20) + "..." : d.label);

simulation.on("tick", () => {
    link
        .attr("x1", d => d.source.x)
        .attr("y1", d => d.source.y)
        .attr("x2", d => d.target.x)
        .attr("y2", d => d.target.y);

    node
        .attr("cx", d => d.x)
        .attr("cy", d => d.y);
        
    label
        .attr("x", d => d.x)
        .attr("y", d => d.y);
});

function drag(simulation) {
    return d3.drag()
        .on("start", (event, d) => {
            if (!event.active) simulation.alphaTarget(0.3).restart();
            d.fx = d.x;
            d.fy = d.y;
        })
        .on("drag", (event, d) => {
            d.fx = event.x;
            d.fy = event.y;
        })
        .on("end", (event, d) => {
            if (!event.active) simulation.alphaTarget(0);
            d.fx = null;
            d.fy = null;
        });
}

let selectedNode = null;

node.on("click", (event, d) => {
    event.stopPropagation();
    selectNode(d);
});

node.on("keydown", (event, d) => {
    if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        selectNode(d);
    }
});

svg.on("click", () => {
    clearSelection();
});

function selectNode(d) {
    selectedNode = d;
    node.classed("active-focus", n => n.id === d.id);
    
    const connectedNodeIds = new Set([d.id]);
    link.classed("active", l => {
        const isConnected = l.source.id === d.id || l.target.id === d.id;
        if (isConnected) {
            connectedNodeIds.add(l.source.id);
            connectedNodeIds.add(l.target.id);
        }
        return isConnected;
    });
    
    node.style("opacity", n => connectedNodeIds.has(n.id) ? 1.0 : 0.25);
    label.style("opacity", n => connectedNodeIds.has(n.id) ? 1.0 : 0.1);
    
    populateSidebar(d);
    
    const transform = d3.zoomTransform(svg.node());
    svg.transition().duration(750).call(
        zoom.transform,
        d3.zoomIdentity.translate(width/2 - d.x * transform.k, height/2 - d.y * transform.k).scale(transform.k)
    );
}

function clearSelection() {
    selectedNode = null;
    node.classed("active-focus", false).style("opacity", 1.0);
    label.style("opacity", 1.0);
    link.classed("active", false);
    
    d3.select("#details-panel").html(`
        <div class="empty-details">
            Select a node in the graph to view its connections and properties.
        </div>
    `);
}

function populateSidebar(d) {
    const panel = d3.select("#details-panel");
    panel.html("");
    
    const wrapper = panel.append("div").attr("class", "node-details");
    wrapper.append("div").attr("class", `detail-badge badge-${d.type}`).text(d.type);
    wrapper.append("h2").attr("class", "detail-title").text(d.label);
    
    const meta = wrapper.append("div").attr("class", "detail-meta");
    meta.append("div").attr("class", "meta-row").html(`<span class="meta-label">ID:</span><span class="meta-value">${d.id}</span>`);
}

const sidebar = d3.select("#sidebar");
const toggleBtn = d3.select("#toggle-sidebar");
toggleBtn.on("click", () => {
    const collapsed = sidebar.classed("collapsed");
    sidebar.classed("collapsed", !collapsed);
    toggleBtn.classed("collapsed", !collapsed);
    toggleBtn.attr("aria-expanded", collapsed ? "true" : "false");
});

d3.select("#btn-reset").on("click", () => {
    svg.transition().duration(750).call(
        zoom.transform,
        d3.zoomIdentity
    );
});

// Interactive Search with Center & Highlight
d3.select("#search-input").on("input", function() {
    const q = this.value.toLowerCase();
    if (!q) {
        node.style("opacity", 1.0);
        label.style("opacity", 1.0);
        return;
    }
    
    let matchedNode = null;
    node.style("opacity", n => {
        const match = n.label.toLowerCase().includes(q);
        if (match && !matchedNode) {
            matchedNode = n;
        }
        return match ? 1.0 : 0.2;
    });
    label.style("opacity", n => n.label.toLowerCase().includes(q) ? 1.0 : 0.1);

    if (matchedNode) {
        selectNode(matchedNode);
    }
});
