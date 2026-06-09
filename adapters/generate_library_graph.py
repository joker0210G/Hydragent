import os
import sqlite3
import json

def generate_graph():
    base_dir = os.path.dirname(os.path.abspath(__file__))
    db_path = os.path.abspath(os.path.join(base_dir, "..", "data", "sessions.db"))
    output_html = os.path.abspath(os.path.join(base_dir, "miniapp", "graph.html"))
    
    if not os.path.exists(db_path):
        # Create directory and empty list fallback if db not initialized
        os.makedirs(os.path.dirname(output_html), exist_ok=True)
        write_empty_fallback(output_html)
        return
        
    conn = sqlite3.connect(db_path)
    cursor = conn.cursor()
    
    # Check if tables exist
    cursor.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='nodes'")
    if not cursor.fetchone():
        conn.close()
        write_empty_fallback(output_html)
        return
        
    # Fetch nodes
    cursor.execute("SELECT node_id, type, label, properties FROM nodes")
    nodes = []
    for row in cursor.fetchall():
        props = {}
        if row[3]:
            try:
                props = json.loads(row[3])
            except:
                pass
        nodes.append({
            "id": row[0],
            "type": row[1],
            "label": row[2],
            "properties": props
        })
        
    # Fetch edges
    cursor.execute("SELECT edge_id, source_node_id, target_node_id, relation_type, weight FROM edges")
    links = []
    for row in cursor.fetchall():
        links.append({
            "id": row[0],
            "source": row[1],
            "target": row[2],
            "relation": row[3],
            "weight": row[4]
        })
        
    conn.close()
    
    # Generate interactive D3.js HTML
    html_content = f"""<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Library Knowledge Graph</title>
    <script src="https://d3js.org/d3.v7.min.js"></script>
    <link href="https://fonts.googleapis.com/css2?family=Outfit:wght@300;400;600;800&display=swap" rel="stylesheet">
    <style>
        * {{
            box-sizing: border-box;
            margin: 0;
            padding: 0;
        }}
        body {{
            font-family: 'Outfit', sans-serif;
            background: #0f0c1b;
            color: #ffffff;
            overflow: hidden;
            width: 100vw;
            height: 100vh;
        }}
        #canvas {{
            width: 100%;
            height: 100%;
        }}
        .node {{
            stroke: #fff;
            stroke-width: 1.5px;
            cursor: pointer;
            transition: r 0.2s, stroke-width 0.2s;
        }}
        .node:hover {{
            stroke-width: 3px;
        }}
        .link {{
            stroke: #5f5f7a;
            stroke-opacity: 0.6;
            stroke-width: 1.5px;
            fill: none;
        }}
        .edge-label {{
            fill: #8b8ba9;
            font-size: 10px;
            pointer-events: none;
        }}
        .tooltip {{
            position: absolute;
            bottom: 20px;
            left: 20px;
            background: rgba(23, 19, 44, 0.9);
            border: 1px solid rgba(138, 43, 226, 0.4);
            border-radius: 12px;
            padding: 15px;
            max-width: 320px;
            box-shadow: 0 8px 32px 0 rgba(0, 0, 0, 0.5);
            backdrop-filter: blur(10px);
            pointer-events: none;
            opacity: 0;
            transition: opacity 0.3s;
        }}
        .tooltip-title {{
            font-size: 16px;
            font-weight: 600;
            margin-bottom: 5px;
            color: #c5a3ff;
        }}
        .tooltip-type {{
            font-size: 11px;
            text-transform: uppercase;
            font-weight: 800;
            letter-spacing: 1px;
            color: #ff7ebb;
            margin-bottom: 10px;
        }}
        .tooltip-props {{
            font-size: 12px;
            color: #a0a0c0;
            line-height: 1.4;
        }}
        .legend {{
            position: absolute;
            top: 20px;
            right: 20px;
            background: rgba(23, 19, 44, 0.8);
            border: 1px solid rgba(255, 255, 255, 0.1);
            border-radius: 8px;
            padding: 10px;
            display: flex;
            flex-direction: column;
            gap: 8px;
            font-size: 12px;
        }}
        .legend-item {{
            display: flex;
            align-items: center;
            gap: 8px;
        }}
        .legend-color {{
            width: 12px;
            height: 12px;
            border-radius: 50%;
        }}
        .btn-refresh {{
            position: absolute;
            top: 20px;
            left: 20px;
            background: #8a2be2;
            border: none;
            border-radius: 6px;
            color: white;
            padding: 8px 12px;
            font-size: 12px;
            cursor: pointer;
            font-weight: 600;
        }}
        .btn-refresh:hover {{
            background: #a34dfb;
        }}
    </style>
</head>
<body>
    <button class="btn-refresh" onclick="location.reload()">🔄 Refresh Graph</button>
    <div class="legend">
        <div class="legend-item"><div class="legend-color" style="background: #9d4edd;"></div> Shelf</div>
        <div class="legend-item"><div class="legend-color" style="background: #4cc9f0;"></div> Book</div>
        <div class="legend-item"><div class="legend-color" style="background: #4caf50;"></div> Page</div>
    </div>
    
    <div id="tooltip" class="tooltip">
        <div id="tooltip-title" class="tooltip-title">Node Title</div>
        <div id="tooltip-type" class="tooltip-type">Shelf</div>
        <div id="tooltip-props" class="tooltip-props">Properties</div>
    </div>

    <svg id="canvas"></svg>

    <script>
        const graphData = {{
            "nodes": {json.dumps(nodes)},
            "links": {json.dumps(links)}
        }};

        const width = window.innerWidth;
        const height = window.innerHeight;

        const svg = d3.select("#canvas")
            .attr("width", width)
            .attr("height", height);

        // Zoom & Pan group
        const g = svg.append("g");
        svg.call(d3.zoom().on("zoom", (event) => {{
            g.attr("transform", event.transform);
        }}));

        // Color mapper
        const colorMap = {{
            "shelf": "#9d4edd",
            "book": "#4cc9f0",
            "page": "#4caf50"
        }};

        const sizeMap = {{
            "shelf": 22,
            "book": 16,
            "page": 10
        }};

        // Force simulation
        const simulation = d3.forceSimulation(graphData.nodes)
            .force("link", d3.forceLink(graphData.links).id(d => d.id).distance(100))
            .force("charge", d3.forceManyBody().strength(-300))
            .force("center", d3.forceCenter(width / 2, height / 2))
            .force("collision", d3.forceCollide().radius(d => sizeMap[d.type] + 5));

        // Draw links
        const link = g.append("g")
            .selectAll("line")
            .data(graphData.links)
            .enter().append("line")
            .attr("class", "link");

        // Draw nodes
        const node = g.append("g")
            .selectAll("circle")
            .data(graphData.nodes)
            .enter().append("circle")
            .attr("class", "node")
            .attr("r", d => sizeMap[d.type])
            .attr("fill", d => colorMap[d.type] || "#ffffff")
            .call(d3.drag()
                .on("start", dragstarted)
                .on("drag", dragged)
                .on("end", dragended))
            .on("mouseover", showTooltip)
            .on("mouseout", hideTooltip);

        // Text labels for nodes
        const label = g.append("g")
            .selectAll("text")
            .data(graphData.nodes)
            .enter().append("text")
            .attr("dx", d => sizeMap[d.type] + 4)
            .attr("dy", ".35em")
            .attr("fill", "#ffffff")
            .style("font-size", "11px")
            .style("pointer-events", "none")
            .text(d => d.label);

        simulation.on("tick", () => {{
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
        }});

        function showTooltip(event, d) {{
            const tooltip = document.getElementById("tooltip");
            document.getElementById("tooltip-title").innerText = d.label;
            document.getElementById("tooltip-type").innerText = d.type;
            
            let propStr = `ID: ${{d.id}}<br>`;
            if (d.properties) {{
                for (const [k, v] of Object.entries(d.properties)) {{
                    propStr += `${{k}}: ${{JSON.stringify(v)}}<br>`;
                }}
            }}
            document.getElementById("tooltip-props").innerHTML = propStr;
            tooltip.style.opacity = 1;
        }}

        function hideTooltip() {{
            document.getElementById("tooltip").style.opacity = 0;
        }}

        function dragstarted(event, d) {{
            if (!event.active) simulation.alphaTarget(0.3).restart();
            d.fx = d.x;
            d.fy = d.y;
        }}

        function dragged(event, d) {{
            d.fx = event.x;
            d.fy = event.y;
        }}

        function dragended(event, d) {{
            if (!event.active) simulation.alphaTarget(0);
            d.fx = null;
            d.fy = null;
        }}
    </script>
</body>
</html>
"""
    with open(output_html, "w", encoding="utf-8") as f:
        f.write(html_content)

def write_empty_fallback(path):
    with open(path, "w", encoding="utf-8") as f:
        f.write("""<!DOCTYPE html>
<html>
<body style="background:#0f0c1b; color:#fff; font-family:sans-serif; text-align:center; padding-top:100px;">
    <h2>No Library graph data available yet.</h2>
    <p>Create shelves, books, and pages first to visualize your interconnected library map.</p>
</body>
</html>""")

if __name__ == "__main__":
    generate_graph()
