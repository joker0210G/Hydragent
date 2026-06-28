import argparse
import sqlite3
import os
import json
import shutil
import logging
from datetime import datetime
from .config import Config
from .builder import LibraryGraphBuilder

def setup_logging():
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s - %(name)s - %(levelname)s - %(message)s"
    )

def main():
    setup_logging()
    logger = logging.getLogger("hydragent.graph")

    parser = argparse.ArgumentParser(description="Hydragent Library Graph Generator")
    parser.add_argument("--db-path", type=str, help="Path to the SQLite database")
    parser.add_argument("--output", type=str, help="Path to write the output HTML file")
    
    args = parser.parse_args()

    # Determine default paths if not provided
    db_path = args.db_path
    if not db_path:
        home = os.path.expanduser("~")
        db_path = os.path.abspath(os.path.join(home, ".hydragent", "data", "sessions.db"))

    output_path = args.output
    if not output_path:
        home = os.path.expanduser("~")
        output_path = os.path.abspath(os.path.join(home, ".hydragent", "data", "graph.html"))

    logger.info("Using database at: %s", db_path)
    logger.info("Writing output to: %s", output_path)

    if not os.path.exists(db_path):
        logger.error("Database file not found: %s", db_path)
        return

    start_time = datetime.now()

    conn = sqlite3.connect(db_path)
    config = Config()
    builder = LibraryGraphBuilder(conn, config)
    
    logger.info("Clustering and building graph...")
    builder.build_and_cluster_graph()

    # Fetch all nodes and edges to write the HTML output
    nodes = builder.node_dao.load_all_nodes()
    active_node_ids = {n.id for n in nodes}
    edges = [e for e in builder.edge_dao.load_all_edges() if e.source in active_node_ids and e.target in active_node_ids]

    # Task 5.3: Soft limit check for performance on large graphs
    max_nodes_limit = 5000
    if len(nodes) > max_nodes_limit:
        logger.warning(
            "Graph has %d nodes, exceeding the performance limit of %d. Rendering may be sluggish.", 
            len(nodes), 
            max_nodes_limit
        )


    # Map to D3 compatible JSON
    nodes_json = []
    for n in nodes:
        nodes_json.append({
            "id": n.id,
            "type": n.type,
            "label": n.label,
            "properties": n.properties
        })

    links_json = []
    for e in edges:
        links_json.append({
            "id": e.id,
            "source": e.source,
            "target": e.target,
            "relation": e.relation,
            "weight": e.weight
        })

    graph_data = {
        "nodes": nodes_json,
        "links": links_json
    }

    # Load template and write assets
    current_dir = os.path.dirname(os.path.abspath(__file__))
    templates_dir = os.path.join(current_dir, "templates")
    template_path = os.path.join(templates_dir, "library_graph.html")
    
    if os.path.exists(template_path):
        # 1. Write the main HTML shell
        with open(template_path, "r", encoding="utf-8") as f:
            template_content = f.read()
        
        output_content = template_content.replace("{{GRAPH_DATA_JSON}}", json.dumps(graph_data))
        
        output_dir = os.path.dirname(output_path)
        os.makedirs(output_dir, exist_ok=True)
        
        with open(output_path, "w", encoding="utf-8") as f:
            f.write(output_content)
            
        # 2. Copy externalized CSS and JS to the output directory
        for asset in ["library_graph.css", "library_graph.js"]:
            src = os.path.join(templates_dir, asset)
            dst = os.path.join(output_dir, asset)
            if os.path.exists(src):
                shutil.copy2(src, dst)
                logger.info("Copied asset %s to %s", asset, dst)
                
        logger.info("Graph successfully written to %s", output_path)
    else:
        logger.error("Template not found: %s", template_path)

    # Write manifest
    duration = (datetime.now() - start_time).total_seconds() * 1000
    manifest = {
        "generated_at": datetime.now().isoformat(),
        "node_count": len(nodes),
        "edge_count": len(edges),
        "build_duration_ms": duration
    }
    manifest_path = os.path.join(os.path.dirname(output_path), "graph_manifest.json")
    with open(manifest_path, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)

    conn.close()

if __name__ == "__main__":
    main()
