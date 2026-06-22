"""
generate_library_graph.py

Hydragent Library Graph Generator — 75% Local Graphify Pipeline
================================================================
Per design spec §2 (Cost-Effective Ingestion Loop):

  [Draft Paper] ──► [Librarian LLM - 25%] ──► Extracts Summary & Personality
                                                      │
                                          (Passes Page nodes to Graphify)
                                                      ▼
  [Customized Graphify (Local - 75%)] ◄──────────────┘
          │
          ├─► Document-Free Mode: skips .md / .txt / raw docs (no LLM costs)
          ├─► Dynamic Node Ingestion: reads Page nodes from SQLite
          ├─► Graphify Clustering (Leiden/Louvain → Books & Shelves)
          └─► Writes Book/Shelf nodes and belongs_to/sits_on edges to SQLite

Run this script after the dream cycle to rebuild the Library graph and
regenerate the interactive D3.js visualisation (adapters/miniapp/graph.html).
"""

import os
import sqlite3
import json
import uuid
import hashlib

# ── Graphify local clustering (75% weight) ─────────────────────────────────
# Document-Free Mode: we import only the clustering module.
# We deliberately do NOT call graphify.detect() on the filesystem — that
# would ingest raw markdown/docs and trigger expensive LLM extraction.
# Instead, we feed our own memory nodes into NetworkX directly.
try:
    from graphify.cluster import cluster as graphify_cluster
    import networkx as nx
    GRAPHIFY_AVAILABLE = True
except ImportError:
    GRAPHIFY_AVAILABLE = False
    print("[WARN] graphify or networkx not available — skipping Louvain clustering")


# ── Helpers ────────────────────────────────────────────────────────────────

def _stable_id(*parts: str) -> str:
    """Generate a stable, deterministic node ID from string parts."""
    return hashlib.md5("-".join(parts).encode()).hexdigest()[:16]


def _get_db_path():
    # 1. Check explicit environment override
    if "HYDRAGENT_HOME" in os.environ:
        path = os.path.abspath(os.path.join(os.environ["HYDRAGENT_HOME"], "data", "sessions.db"))
        if os.path.exists(path):
            return path
    
    # 2. Check standard Windows/Unix home directory path
    home = os.path.expanduser("~")
    path = os.path.abspath(os.path.join(home, ".hydragent", "data", "sessions.db"))
    if os.path.exists(path):
        return path

    # 3. Local relative fallback
    base_dir = os.path.dirname(os.path.abspath(__file__))
    return os.path.abspath(os.path.join(base_dir, "..", "data", "sessions.db"))


def _get_output_html():
    base_dir = os.path.dirname(os.path.abspath(__file__))
    return os.path.abspath(os.path.join(base_dir, "miniapp", "graph.html"))



# ── Dynamic Node Ingestion API ─────────────────────────────────────────────

def load_page_nodes(conn) -> list[dict]:
    """
    Load Page nodes from the Library's nodes table.
    These were written by the dream cycle (LLM 25% step).
    """
    cursor = conn.cursor()
    cursor.execute("SELECT node_id, label, properties FROM nodes WHERE type = 'page'")
    pages = []
    for node_id, label, props_str in cursor.fetchall():
        props = {}
        if props_str:
            try:
                props = json.loads(props_str)
            except Exception:
                pass
        pages.append({"id": node_id, "label": label, "properties": props})
    return pages


def load_page_tags(conn, page_id: str) -> list[str]:
    """
    Load tags associated with a page's semantic memories.
    Used to build edges between pages that share topics.
    """
    cursor = conn.cursor()
    cursor.execute(
        """SELECT mt.tag FROM memory_tags mt
           JOIN semantic_memories sm ON mt.memory_id = sm.id
           WHERE sm.page_id = ?""",
        (page_id,)
    )
    return [row[0] for row in cursor.fetchall()]


def write_node(conn, node_id: str, node_type: str, label: str, properties: dict):
    cursor = conn.cursor()
    cursor.execute(
        "INSERT OR REPLACE INTO nodes (node_id, type, label, properties) VALUES (?, ?, ?, ?)",
        (node_id, node_type, label, json.dumps(properties))
    )


def write_edge(conn, source: str, target: str, relation: str, weight: float = 1.0):
    edge_id = f"{source}-{relation}-{target}"
    cursor = conn.cursor()
    cursor.execute(
        "INSERT OR REPLACE INTO edges (edge_id, source_node_id, target_node_id, relation_type, weight) VALUES (?, ?, ?, ?, ?)",
        (edge_id, source, target, relation, weight)
    )


# ── Graphify Clustering (75% Weight) ──────────────────────────────────────

def cluster_pages_into_books(conn, pages: list[dict]) -> dict[str, list[str]]:
    """
    Build a page-to-page NetworkX graph weighted by shared tags, then run
    Leiden/Louvain community detection via graphify.cluster to group Pages
    into Books (topic clusters).

    Returns: {community_id -> [page_id, ...]}
    """
    if not GRAPHIFY_AVAILABLE or len(pages) < 2:
        return {}

    # Build page-tag index
    page_tags: dict[str, set[str]] = {}
    for page in pages:
        tags = set(load_page_tags(conn, page["id"]))
        page_tags[page["id"]] = tags

    # Build weighted NetworkX graph: edge weight = |shared tags|
    G = nx.Graph()
    for page in pages:
        G.add_node(page["id"], label=page["label"])

    page_ids = [p["id"] for p in pages]
    for i, pid_a in enumerate(page_ids):
        for pid_b in page_ids[i + 1:]:
            shared = page_tags.get(pid_a, set()) & page_tags.get(pid_b, set())
            if shared:
                G.add_edge(pid_a, pid_b, weight=len(shared))

    # Run community detection (Leiden algorithm via graphify)
    communities = graphify_cluster(G)  # {community_id: [node_ids]}
    return communities


def cluster_books_into_shelves(conn, book_nodes: list[dict]) -> dict[str, list[str]]:
    """
    Build a book-to-book NetworkX graph weighted by shared page membership,
    then run community detection to group Books into Shelves (domain clusters).

    Returns: {community_id -> [book_id, ...]}
    """
    if not GRAPHIFY_AVAILABLE or len(book_nodes) < 2:
        return {}

    # For each book, get the page IDs that belong to it
    book_pages: dict[str, set[str]] = {}
    cursor = conn.cursor()
    for book in book_nodes:
        cursor.execute(
            "SELECT source_node_id FROM edges WHERE target_node_id = ? AND relation_type = 'belongs_to'",
            (book["id"],)
        )
        book_pages[book["id"]] = {row[0] for row in cursor.fetchall()}

    G = nx.Graph()
    for book in book_nodes:
        G.add_node(book["id"], label=book["label"])

    book_ids = [b["id"] for b in book_nodes]
    for i, bid_a in enumerate(book_ids):
        for bid_b in book_ids[i + 1:]:
            shared = book_pages.get(bid_a, set()) & book_pages.get(bid_b, set())
            if shared:
                G.add_edge(bid_a, bid_b, weight=len(shared))

    communities = graphify_cluster(G)
    return communities


# ── Main Pipeline ──────────────────────────────────────────────────────────

def generate_graph():
    db_path = _get_db_path()
    output_html = _get_output_html()

    if not os.path.exists(db_path):
        os.makedirs(os.path.dirname(output_html), exist_ok=True)
        write_empty_fallback(output_html)
        return

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row

    cursor = conn.cursor()

    # Check if tables exist
    cursor.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='nodes'")
    if not cursor.fetchone():
        conn.close()
        write_empty_fallback(output_html)
        return

    # ── Step 1: Load Page nodes (written by dream cycle LLM step) ───────────
    pages = load_page_nodes(conn)
    print(f"[generate_library_graph] Loaded {len(pages)} Page nodes from Library")

    book_nodes_created: list[dict] = []
    if GRAPHIFY_AVAILABLE and pages:
        print("[generate_library_graph] Running Leiden/Louvain clustering: Pages -> Books")
        communities = cluster_pages_into_books(conn, pages)

        for cid, page_ids in communities.items():
            if not page_ids:
                continue
            # Name the Book by the first page's label (truncated)
            first_label = next(
                (p["label"] for p in pages if p["id"] in page_ids),
                f"Topic Cluster {cid}"
            )
            book_label = first_label[:60] + "..." if len(first_label) > 60 else first_label
            book_id = f"book-{_stable_id(str(cid), *sorted(page_ids))}"

            write_node(conn, book_id, "book", book_label, {
                "community_id": cid,
                "page_count": len(page_ids),
                "source": "graphify_cluster",
            })
            book_nodes_created.append({"id": book_id, "label": book_label})

            # Write belongs_to edges: Page -> Book
            for pid in page_ids:
                write_edge(conn, pid, book_id, "belongs_to", weight=1.0)

        print(f"[generate_library_graph] Created {len(book_nodes_created)} Book nodes")

    # ── Step 3: Louvain clustering -> Books into Shelves ─────────────────────
    if GRAPHIFY_AVAILABLE and book_nodes_created:
        print("[generate_library_graph] Running Leiden/Louvain clustering: Books -> Shelves")
        shelf_communities = cluster_books_into_shelves(conn, book_nodes_created)

        shelf_count = 0
        for cid, book_ids in shelf_communities.items():
            if not book_ids:
                continue
            first_label = next(
                (b["label"] for b in book_nodes_created if b["id"] in book_ids),
                f"Domain {cid}"
            )
            shelf_label = first_label[:60] + "…" if len(first_label) > 60 else first_label
            shelf_id = f"shelf-{_stable_id(str(cid), *sorted(book_ids))}"

            write_node(conn, shelf_id, "shelf", shelf_label, {
                "community_id": cid,
                "book_count": len(book_ids),
                "source": "graphify_cluster",
            })

            # Write sits_on edges: Book → Shelf
            for bid in book_ids:
                write_edge(conn, bid, shelf_id, "sits_on", weight=1.0)

            shelf_count += 1

        print(f"[generate_library_graph] Created {shelf_count} Shelf nodes")

    conn.commit()

    # ── Step 4: Fetch ALL nodes & edges for D3 visualisation ─────────────────
    cursor.execute("SELECT node_id, type, label, properties FROM nodes")
    all_nodes = []
    for row in cursor.fetchall():
        props = {}
        if row["properties"]:
            try:
                props = json.loads(row["properties"])
            except Exception:
                pass
        all_nodes.append({
            "id": row["node_id"],
            "type": row["type"],
            "label": row["label"],
            "properties": props,
        })

    cursor.execute(
        "SELECT edge_id, source_node_id, target_node_id, relation_type, weight FROM edges"
    )
    all_links = []
    for row in cursor.fetchall():
        all_links.append({
            "id": row["edge_id"],
            "source": row["source_node_id"],
            "target": row["target_node_id"],
            "relation": row["relation_type"],
            "weight": row["weight"],
        })

    conn.close()

    print(f"[generate_library_graph] Rendering D3 graph: {len(all_nodes)} nodes, {len(all_links)} edges")
    _write_d3_html(output_html, all_nodes, all_links)
    print(f"[generate_library_graph] Graph written to {output_html}")


# ── D3 Visualisation ──────────────────────────────────────────────────────

def _write_d3_html(path: str, nodes: list[dict], links: list[dict]):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    html = f"""<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Hydragent Library Knowledge Graph</title>
    <meta name="description" content="Interactive Library knowledge graph — Shelves, Books, Pages, and Tags visualised as a force-directed graph.">
    <script src="https://d3js.org/d3.v7.min.js"></script>
    <link href="https://fonts.googleapis.com/css2?family=Outfit:wght@300;400;500;600;700;800&display=swap" rel="stylesheet">
    <style>
        * {{ box-sizing: border-box; margin: 0; padding: 0; }}
        body {{
            font-family: 'Outfit', sans-serif;
            background: #08070e;
            color: #f1f0f7;
            overflow: hidden;
            width: 100vw;
            height: 100vh;
            display: flex;
            position: relative;
        }}
        
        /* Sidebar Styling */
        #sidebar {{
            position: absolute;
            left: 0;
            top: 0;
            width: 380px;
            height: 100vh;
            background: rgba(13, 11, 23, 0.96);
            border-right: 1px solid rgba(255, 255, 255, 0.08);
            backdrop-filter: blur(20px);
            z-index: 10;
            display: flex;
            flex-direction: column;
            box-shadow: 10px 0 30px rgba(0, 0, 0, 0.5);
            transition: transform 0.3s cubic-bezier(0.4, 0, 0.2, 1);
        }}
        
        #sidebar.collapsed {{
            transform: translateX(-100%);
        }}
        
        .sidebar-header {{
            padding: 24px;
            border-bottom: 1px solid rgba(255, 255, 255, 0.08);
        }}
        
        .sidebar-title {{
            font-size: 20px;
            font-weight: 700;
            background: linear-gradient(135deg, #a855f7, #3b82f6);
            -webkit-background-clip: text;
            -webkit-text-fill-color: transparent;
            display: flex;
            align-items: center;
            gap: 10px;
        }}
        
        .sidebar-subtitle {{
            font-size: 11px;
            color: #7c7a93;
            margin-top: 4px;
            text-transform: uppercase;
            letter-spacing: 1.5px;
        }}
        
        .search-container {{
            padding: 16px 24px;
            border-bottom: 1px solid rgba(255, 255, 255, 0.08);
        }}
        
        .search-box {{
            width: 100%;
            padding: 12px 16px;
            background: rgba(255, 255, 255, 0.05);
            border: 1px solid rgba(255, 255, 255, 0.1);
            border-radius: 8px;
            color: #fff;
            font-family: inherit;
            font-size: 14px;
            outline: none;
            transition: all 0.3s ease;
        }}
        
        .search-box:focus {{
            border-color: #a855f7;
            background: rgba(255, 255, 255, 0.08);
            box-shadow: 0 0 10px rgba(168, 85, 247, 0.2);
        }}
        
        .filter-section {{
            padding: 16px 24px;
            border-bottom: 1px solid rgba(255, 255, 255, 0.08);
        }}
        
        .section-title {{
            font-size: 12px;
            font-weight: 600;
            color: #8c8aa2;
            text-transform: uppercase;
            letter-spacing: 1px;
            margin-bottom: 12px;
        }}
        
        .filter-grid {{
            display: grid;
            grid-template-columns: repeat(2, 1fr);
            gap: 8px;
        }}
        
        .filter-btn {{
            display: flex;
            align-items: center;
            gap: 8px;
            padding: 8px 12px;
            background: rgba(255, 255, 255, 0.04);
            border: 1px solid rgba(255, 255, 255, 0.08);
            border-radius: 6px;
            cursor: pointer;
            font-size: 12px;
            color: #c1bfd6;
            transition: all 0.2s ease;
        }}
        
        .filter-btn:hover {{
            background: rgba(255, 255, 255, 0.08);
            color: #fff;
        }}
        
        .filter-btn.active {{
            background: rgba(168, 85, 247, 0.15);
            border-color: rgba(168, 85, 247, 0.4);
            color: #e9d5ff;
        }}
        
        .details-container {{
            flex-grow: 1;
            padding: 24px;
            overflow-y: auto;
            display: flex;
            flex-direction: column;
            gap: 16px;
        }}
        
        .empty-details {{
            color: #646279;
            font-size: 14px;
            text-align: center;
            margin-top: 40px;
            font-style: italic;
        }}
        
        .node-details {{
            display: flex;
            flex-direction: column;
            gap: 16px;
        }}
        
        .detail-badge {{
            align-self: flex-start;
            padding: 4px 10px;
            border-radius: 20px;
            font-size: 10px;
            font-weight: 800;
            text-transform: uppercase;
            letter-spacing: 1px;
        }}
        
        .badge-shelf {{ background: rgba(168, 85, 247, 0.2); color: #c084fc; border: 1px solid rgba(168, 85, 247, 0.4); }}
        .badge-book {{ background: rgba(59, 130, 246, 0.2); color: #60a5fa; border: 1px solid rgba(59, 130, 246, 0.4); }}
        .badge-page {{ background: rgba(16, 185, 129, 0.2); color: #34d399; border: 1px solid rgba(16, 185, 129, 0.4); }}
        .badge-tag {{ background: rgba(245, 158, 11, 0.2); color: #fbbf24; border: 1px solid rgba(245, 158, 11, 0.4); }}
        
        .detail-title {{
            font-size: 18px;
            font-weight: 600;
            color: #fff;
            line-height: 1.4;
        }}
        
        .detail-props-table {{
            width: 100%;
            border-collapse: collapse;
            font-size: 13px;
        }}
        
        .detail-props-table th, .detail-props-table td {{
            padding: 8px 0;
            text-align: left;
            border-bottom: 1px solid rgba(255, 255, 255, 0.05);
        }}
        
        .detail-props-table th {{
            color: #7c7a93;
            font-weight: 500;
            width: 90px;
        }}
        
        .detail-props-table td {{
            color: #d1d0db;
            word-break: break-word;
        }}
        
        .neighbors-list {{
            display: flex;
            flex-direction: column;
            gap: 6px;
            margin-top: 8px;
        }}
        
        .neighbor-item {{
            padding: 8px 12px;
            background: rgba(255, 255, 255, 0.03);
            border: 1px solid rgba(255, 255, 255, 0.05);
            border-radius: 6px;
            font-size: 12px;
            cursor: pointer;
            transition: all 0.2s ease;
            display: flex;
            align-items: center;
            justify-content: space-between;
        }}
        
        .neighbor-item:hover {{
            background: rgba(255, 255, 255, 0.07);
            border-color: rgba(255, 255, 255, 0.1);
        }}
        
        /* Main Viewport */
        #viewport {{
            width: 100%;
            height: 100vh;
            position: relative;
            transition: padding-left 0.3s cubic-bezier(0.4, 0, 0.2, 1);
            padding-left: 380px;
        }}
        
        #viewport.full-width {{
            padding-left: 0;
        }}
        
        #canvas {{
            width: 100%;
            height: 100%;
            display: block;
        }}
        
        /* Floating Toggle Button */
        #sidebar-toggle {{
            position: absolute;
            left: 396px;
            top: 24px;
            z-index: 15;
            background: rgba(18, 16, 31, 0.9);
            border: 1px solid rgba(255, 255, 255, 0.1);
            border-radius: 8px;
            color: #fff;
            width: 40px;
            height: 40px;
            display: flex;
            align-items: center;
            justify-content: center;
            cursor: pointer;
            backdrop-filter: blur(8px);
            box-shadow: 0 4px 15px rgba(0,0,0,0.3);
            transition: all 0.3s cubic-bezier(0.4, 0, 0.2, 1);
        }}

        #sidebar-toggle.collapsed-toggle {{
            left: 24px;
        }}
        
        /* Floating HUD Controls */
        .hud-panel {{
            position: absolute;
            background: rgba(18, 16, 31, 0.85);
            border: 1px solid rgba(255, 255, 255, 0.08);
            border-radius: 12px;
            padding: 16px;
            backdrop-filter: blur(12px);
            z-index: 5;
            box-shadow: 0 10px 30px rgba(0, 0, 0, 0.4);
        }}
        
        #hud-controls {{
            top: 24px;
            left: 452px;
            display: flex;
            gap: 8px;
            transition: left 0.3s cubic-bezier(0.4, 0, 0.2, 1);
        }}
        
        #sidebar-toggle.collapsed-toggle ~ #viewport #hud-controls {{
            left: 80px;
        }}
        
        #hud-tuning {{
            bottom: 24px;
            right: 24px;
            width: 280px;
            display: flex;
            flex-direction: column;
            gap: 12px;
        }}
        
        .tuning-row {{
            display: flex;
            flex-direction: column;
            gap: 4px;
        }}
        
        .tuning-label {{
            font-size: 11px;
            color: #8c8aa2;
            display: flex;
            justify-content: space-between;
        }}
        
        .tuning-slider {{
            width: 100%;
            accent-color: #a855f7;
            height: 4px;
            background: rgba(255, 255, 255, 0.1);
            border-radius: 2px;
            cursor: pointer;
        }}
        
        .btn {{
            background: rgba(255, 255, 255, 0.05);
            border: 1px solid rgba(255, 255, 255, 0.1);
            border-radius: 8px;
            color: #fff;
            padding: 10px 16px;
            font-size: 13px;
            font-weight: 600;
            cursor: pointer;
            font-family: inherit;
            display: flex;
            align-items: center;
            gap: 8px;
            transition: all 0.2s ease;
        }}
        
        .btn:hover {{
            background: rgba(255, 255, 255, 0.1);
            border-color: rgba(255, 255, 255, 0.2);
        }}
        
        .btn-primary {{
            background: linear-gradient(135deg, #a855f7, #3b82f6);
            border: none;
        }}
        
        .btn-primary:hover {{
            opacity: 0.9;
        }}
        
        /* Node & Link SVG Styling */
        .node {{
            cursor: pointer;
            transition: filter 0.2s ease, stroke-width 0.2s ease;
        }}
        
        .node-shelf {{ filter: drop-shadow(0 0 8px rgba(168, 85, 247, 0.6)); }}
        .node-book {{ filter: drop-shadow(0 0 6px rgba(59, 130, 246, 0.5)); }}
        .node-page {{ filter: drop-shadow(0 0 4px rgba(16, 185, 129, 0.4)); }}
        .node-tag {{ filter: drop-shadow(0 0 4px rgba(245, 158, 11, 0.4)); }}
        
        .link {{
            stroke-opacity: 0.25;
            transition: stroke-opacity 0.2s ease, stroke-width 0.2s ease;
            fill: none;
        }}
        
        .link.belongs_to {{
            stroke: #3b82f6;
            stroke-dasharray: 4 3;
        }}
        
        .link.sits_on {{
            stroke: #a855f7;
        }}
        
        .link.tag {{
            stroke: #f59e0b;
            stroke-opacity: 0.15;
            stroke-dasharray: 2 4;
        }}
        
        .link.other {{
            stroke: #64748b;
        }}
        
        .label {{
            fill: #c1bfd6;
            font-size: 11px;
            font-weight: 500;
            pointer-events: none;
            text-shadow: 0 0 6px #08070e, 0 0 3px #08070e;
        }}
        
        /* Interactive highlighting states */
        .dimmed {{
            opacity: 0.15 !important;
        }}
        
        .highlighted-node {{
            stroke: #fff !important;
            stroke-width: 3px !important;
        }}
        
        .highlighted-link {{
            stroke-opacity: 0.8 !important;
            stroke-width: 3px !important;
        }}
        
        /* Tooltip style */
        .mini-tooltip {{
            position: absolute;
            background: rgba(13, 11, 23, 0.95);
            border: 1px solid rgba(255, 255, 255, 0.12);
            border-radius: 8px;
            padding: 8px 12px;
            font-size: 12px;
            color: #fff;
            pointer-events: none;
            z-index: 100;
            box-shadow: 0 5px 15px rgba(0,0,0,0.5);
            opacity: 0;
            transition: opacity 0.15s ease;
        }}
        
        /* Responsive Overrides */
        @media (max-width: 768px) {{
            #sidebar {{
                width: 320px;
            }}
            #viewport {{
                padding-left: 0;
            }}
            #sidebar-toggle {{
                left: 336px;
            }}
            #sidebar-toggle.collapsed-toggle {{
                left: 16px;
            }}
            #sidebar-toggle.collapsed-toggle ~ #viewport #hud-controls {{
                left: 72px;
            }}
            #hud-controls {{
                left: 72px;
                top: 16px;
            }}
            #hud-tuning {{
                width: 240px;
                bottom: 16px;
                right: 16px;
            }}
        }}
    </style>
</head>
<body>
    <!-- Sidebar Toggle Button -->
    <button id="sidebar-toggle" title="Toggle Sidebar">◀</button>

    <!-- Sidebar -->
    <div id="sidebar">
        <div class="sidebar-header">
            <div class="sidebar-title">
                <span>📚</span> Hydragent Library
            </div>
            <div class="sidebar-subtitle">Knowledge Graph Explorer</div>
        </div>
        
        <!-- Search -->
        <div class="search-container">
            <input type="text" class="search-box" id="search-input" placeholder="Search nodes or tags...">
        </div>
        
        <!-- Quick Filters -->
        <div class="filter-section">
            <div class="section-title">Node Type Visibility</div>
            <div class="filter-grid">
                <button class="filter-btn active" data-type="shelf">🗄️ Shelves</button>
                <button class="filter-btn active" data-type="book">📘 Books</button>
                <button class="filter-btn active" data-type="page">📄 Pages</button>
                <button class="filter-btn active" data-type="tag">🏷️ Tags</button>
            </div>
        </div>
        
        <!-- Node Details -->
        <div class="details-container" id="details-panel">
            <div class="empty-details">
                Click a node in the graph to view properties, contents, and connected relations.
            </div>
        </div>
    </div>
    
    <!-- Graph Viewport -->
    <div id="viewport">
        <!-- Floating HUD Header Controls -->
        <div class="hud-panel" id="hud-controls">
            <button class="btn btn-primary" onclick="location.reload()">🔄 Refresh Graph</button>
            <button class="btn" id="btn-reset">⬛ Reset View</button>
            <button class="btn" id="btn-tag-toggle">🏷️ Hide Labels</button>
        </div>
        
        <!-- Floating HUD Physics Controls -->
        <div class="hud-panel" id="hud-tuning">
            <div class="section-title">⚙️ Physics Simulation Tuning</div>
            
            <div class="tuning-row">
                <div class="tuning-label">
                    <span>Repulsion (Charge)</span>
                    <span id="val-charge">-350</span>
                </div>
                <input type="range" class="tuning-slider" id="slide-charge" min="-1000" max="-50" value="-350">
            </div>
            
            <div class="tuning-row">
                <div class="tuning-label">
                    <span>Link Distance</span>
                    <span id="val-distance">100</span>
                </div>
                <input type="range" class="tuning-slider" id="slide-distance" min="30" max="300" value="100">
            </div>
            
            <div class="tuning-row">
                <div class="tuning-label">
                    <span>Node Collision</span>
                    <span id="val-collision">12</span>
                </div>
                <input type="range" class="tuning-slider" id="slide-collision" min="2" max="40" value="12">
            </div>
        </div>
        
        <!-- D3 Canvas SVG -->
        <svg id="canvas"></svg>
        
        <!-- Mini floating tooltip -->
        <div id="mini-tooltip" class="mini-tooltip"></div>
    </div>

    <script>
        const graphData = {{
            "nodes": {json.dumps(nodes)},
            "links": {json.dumps(links)}
        }};

        const viewport = document.getElementById("viewport");
        let width = viewport.clientWidth;
        let height = viewport.clientHeight;
        
        const svg = d3.select("#canvas").attr("width", width).attr("height", height);
        const g = svg.append("g");
        
        // Setup markers for directed edges (sits_on & belongs_to)
        svg.append("svg:defs").selectAll("marker")
            .data(["sits_on", "belongs_to", "tag", "other"])
            .enter().append("svg:marker")
            .attr("id", String)
            .attr("viewBox", "0 -5 10 10")
            .attr("refX", 22) // Position marker relative to node boundary
            .attr("refY", 0)
            .attr("markerWidth", 5)
            .attr("markerHeight", 5)
            .attr("orient", "auto")
            .append("svg:path")
            .attr("d", "M0,-3L10,0L0,3")
            .attr("fill", d => d === "sits_on" ? "#a855f7" : (d === "belongs_to" ? "#3b82f6" : "#f59e0b"));

        // Zoom Behavior
        const zoom = d3.zoom()
            .scaleExtent([0.05, 12])
            .on("zoom", e => g.attr("transform", e.transform));
        svg.call(zoom);

        // Center on start
        svg.call(zoom.transform, d3.zoomIdentity.translate(width / 2, height / 2).scale(0.8));

        document.getElementById("btn-reset").addEventListener("click", () => {{
            svg.transition().duration(750)
               .call(zoom.transform, d3.zoomIdentity.translate(width / 2, height / 2).scale(0.8));
        }});

        // Constants
        const colorMap = {{ 
            shelf: "#a855f7", 
            book: "#3b82f6", 
            page: "#10b981", 
            tag: "#f59e0b" 
        }};
        
        const sizeMap = {{ 
            shelf: 26, 
            book: 18, 
            page: 12, 
            tag: 8 
        }};

        const emojiMap = {{
            shelf: "🗄️",
            book: "📘",
            page: "📄",
            tag: "🏷️"
        }};

        // State trackers
        let activeFilters = {{ shelf: true, book: true, page: true, tag: true }};
        let searchQuery = "";
        let selectedNodeId = null;
        let showLabels = true;

        // Force Simulation Initialization
        const sim = d3.forceSimulation(graphData.nodes)
            .force("link", d3.forceLink(graphData.links).id(d => d.id).distance(d => {{
                if (d.relation === "sits_on") return 140;
                if (d.relation === "belongs_to") return 90;
                return 60;
            }}))
            .force("charge", d3.forceManyBody().strength(-350))
            .force("center", d3.forceCenter(width / 2, height / 2))
            .force("collision", d3.forceCollide().radius(d => (sizeMap[d.type] || 10) + 12));

        // Render functions
        let linkGroup = g.append("g").attr("class", "links-group");
        let nodeGroup = g.append("g").attr("class", "nodes-group");
        let labelGroup = g.append("g").attr("class", "labels-group");

        let linkElements, nodeElements, labelElements;

        function updateGraph() {{
            // Apply filtering logic
            const filteredNodes = graphData.nodes.filter(n => {{
                const matchesType = activeFilters[n.type];
                const matchesSearch = searchQuery === "" || 
                    n.label.toLowerCase().includes(searchQuery) || 
                    n.id.toLowerCase().includes(searchQuery) ||
                    (n.type && n.type.toLowerCase().includes(searchQuery));
                return matchesType && matchesSearch;
            }});

            const nodeIds = new Set(filteredNodes.map(n => n.id));
            const filteredLinks = graphData.links.filter(l => {{
                // Both endpoints must exist in visible nodes
                const sourceId = typeof l.source === 'object' ? l.source.id : l.source;
                const targetId = typeof l.target === 'object' ? l.target.id : l.target;
                return nodeIds.has(sourceId) && nodeIds.has(targetId);
            }});

            // 1. Link updates
            linkElements = linkGroup.selectAll("path")
                .data(filteredLinks, d => d.id);
            linkElements.exit().remove();
            linkElements = linkElements.enter().append("path")
                .attr("class", d => `link ${{d.relation || "other"}}`)
                .attr("stroke-width", d => d.weight ? Math.sqrt(d.weight) + 1 : 1.5)
                .attr("marker-end", d => `url(#${{d.relation || "other"}})` )
                .merge(linkElements);

            // 2. Node updates
            nodeElements = nodeGroup.selectAll("circle")
                .data(filteredNodes, d => d.id);
            nodeElements.exit().remove();
            nodeElements = nodeElements.enter().append("circle")
                .attr("class", d => `node node-${{d.type}}`)
                .attr("r", d => sizeMap[d.type] || 10)
                .attr("fill", d => colorMap[d.type] || "#ffffff")
                .attr("stroke", "rgba(255,255,255,0.2)")
                .attr("stroke-width", 1.5)
                .call(d3.drag()
                    .on("start", dragstarted)
                    .on("drag", dragged)
                    .on("end", dragended)
                )
                .on("mouseover", handleMouseOver)
                .on("mousemove", handleMouseMove)
                .on("mouseout", handleMouseOut)
                .on("click", handleNodeClick)
                .merge(nodeElements);

            // Set specific classes/highlights if there is a selected node
            if (selectedNodeId) {{
                applyHighlight(selectedNodeId);
            }} else {{
                clearHighlight();
            }}

            // 3. Label updates
            labelElements = labelGroup.selectAll("text")
                .data(showLabels ? filteredNodes : [], d => d.id);
            labelElements.exit().remove();
            labelElements = labelElements.enter().append("text")
                .attr("class", "label")
                .attr("dx", d => (sizeMap[d.type] || 10) + 6)
                .attr("dy", ".35em")
                .text(d => d.label.length > 25 ? d.label.slice(0, 25) + "..." : d.label)
                .merge(labelElements);

            // Update force simulation data
            sim.nodes(filteredNodes);
            sim.force("link").links(filteredLinks);
            sim.alpha(0.3).restart();
        }}

        // Tick function
        sim.on("tick", () => {{
            if (linkElements) {{
                linkElements.attr("d", d => {{
                    return `M${{d.source.x}},${{d.source.y}} L${{d.target.x}},${{d.target.y}}`;
                }});
            }}
            if (nodeElements) {{
                nodeElements.attr("cx", d => d.x).attr("cy", d => d.y);
            }}
            if (labelElements) {{
                labelElements.attr("x", d => d.x).attr("y", d => d.y);
            }}
        }});

        // Drag behaviors
        function dragstarted(event, d) {{
            if (!event.active) sim.alphaTarget(0.3).restart();
            d.fx = d.x;
            d.fy = d.y;
        }}

        function dragged(event, d) {{
            d.fx = event.x;
            d.fy = event.y;
        }}

        function dragended(event, d) {{
            if (!event.active) sim.alphaTarget(0);
            d.fx = null;
            d.fy = null;
        }}

        // Tooltip & Mouse handlers
        const tooltipEl = document.getElementById("mini-tooltip");
        
        function handleMouseOver(event, d) {{
            tooltipEl.style.opacity = 1;
            tooltipEl.innerHTML = `<strong>${{emojiMap[d.type] || ""}} ${{d.label}}</strong><br><span style="color:#7c7a93;">${{d.type.toUpperCase()}}</span>`;
        }}

        function handleMouseMove(event) {{
            tooltipEl.style.left = (event.pageX + 15) + "px";
            tooltipEl.style.top = (event.pageY - 15) + "px";
        }}

        // Sidebar Toggle Functionality
        const sidebarToggle = document.getElementById("sidebar-toggle");
        const sidebar = document.getElementById("sidebar");
        const viewportEl = document.getElementById("viewport");

        sidebarToggle.addEventListener("click", () => {{
            const isCollapsed = sidebar.classList.toggle("collapsed");
            viewportEl.classList.toggle("full-width", isCollapsed);
            sidebarToggle.classList.toggle("collapsed-toggle", isCollapsed);
            sidebarToggle.textContent = isCollapsed ? "▶" : "◀";
            
            // Recalculate layout sizes after CSS transition
            setTimeout(() => {{
                width = viewportEl.clientWidth;
                height = viewportEl.clientHeight;
                svg.attr("width", width).attr("height", height);
                sim.force("center", d3.forceCenter(width / 2, height / 2)).alpha(0.2).restart();
            }}, 320);
        }});

        function handleMouseOut() {{
            tooltipEl.style.opacity = 0;
        }}

        // Handle Details Panel selection
        function handleNodeClick(event, d) {{
            event.stopPropagation();
            selectNode(d);
        }}

        function selectNode(d) {{
            selectedNodeId = d.id;
            
            // Highlight connections
            applyHighlight(d.id);
            
            // Build neighborhood data
            const neighbors = [];
            graphData.links.forEach(l => {{
                const sId = typeof l.source === 'object' ? l.source.id : l.source;
                const tId = typeof l.target === 'object' ? l.target.id : l.target;
                
                if (sId === d.id) {{
                    const targetNode = graphData.nodes.find(n => n.id === tId);
                    if (targetNode) neighbors.push({{ node: targetNode, type: 'outgoing', rel: l.relation }});
                }} else if (tId === d.id) {{
                    const sourceNode = graphData.nodes.find(n => n.id === sId);
                    if (sourceNode) neighbors.push({{ node: sourceNode, type: 'incoming', rel: l.relation }});
                }}
            }});

            // Build properties list
            let propsHtml = "";
            if (d.properties && Object.keys(d.properties).length > 0) {{
                propsHtml = `
                    <div class="section-title" style="margin-top:8px;">Properties</div>
                    <table class="detail-props-table">
                `;
                for (const [key, val] of Object.entries(d.properties)) {{
                    const displayVal = typeof val === 'object' ? JSON.stringify(val) : val;
                    propsHtml += `
                        <tr>
                            <th>${{key}}</th>
                            <td>${{displayVal}}</td>
                        </tr>
                    `;
                }}
                propsHtml += `</table>`;
            }}

            // Build neighbors HTML
            let neighborsHtml = "";
            if (neighbors.length > 0) {{
                neighborsHtml = `
                    <div class="section-title" style="margin-top:8px;">Connected Relations (${{neighbors.length}})</div>
                    <div class="neighbors-list">
                `;
                neighbors.forEach(n => {{
                    neighborsHtml += `
                        <div class="neighbor-item" onclick="jumpToNode('${{n.node.id}}')">
                            <div>
                                <span>${{emojiMap[n.node.type]}}</span>
                                <strong style="color:#fff; margin-left:4px;">${{n.node.label.substring(0, 20)}}...</strong>
                            </div>
                            <span style="font-size:10px; padding:2px 6px; border-radius:4px; background:rgba(255,255,255,0.06); color:#8c8aa2;">
                                ${{n.rel}} (${{n.type === 'outgoing' ? 'out' : 'in'}})
                            </span>
                        </div>
                    `;
                }});
                neighborsHtml += `</div>`;
            }}

            const detailsPanel = document.getElementById("details-panel");
            detailsPanel.innerHTML = `
                <div class="node-details">
                    <span class="detail-badge badge-${{d.type}}">${{emojiMap[d.type]}} ${{d.type}}</span>
                    <div class="detail-title">${{d.label}}</div>
                    <div style="font-size:12px; color:#7c7a93; word-break:break-all;"><strong>ID:</strong> ${{d.id}}</div>
                    ${{propsHtml}}
                    ${{neighborsHtml}}
                </div>
            `;
        }}

        // Jump to node function
        window.jumpToNode = function(id) {{
            const targetNode = graphData.nodes.find(n => n.id === id);
            if (targetNode) {{
                selectNode(targetNode);
                // Center camera on target node
                svg.transition().duration(750).call(
                    zoom.transform,
                    d3.zoomIdentity.translate(width / 2 - targetNode.x * 1.5, height / 2 - targetNode.y * 1.5).scale(1.5)
                );
            }}
        }};

        // Highlighting Logic
        function applyHighlight(nodeId) {{
            // Find neighbors
            const connectedNodes = new Set();
            connectedNodes.add(nodeId);

            graphData.links.forEach(l => {{
                const sId = typeof l.source === 'object' ? l.source.id : l.source;
                const tId = typeof l.target === 'object' ? l.target.id : l.target;
                
                if (sId === nodeId) {{
                    connectedNodes.add(tId);
                }} else if (tId === nodeId) {{
                    connectedNodes.add(sId);
                }}
            }});

            // Dim others, highlight targets
            nodeElements.classed("dimmed", n => !connectedNodes.has(n.id))
                        .classed("highlighted-node", n => n.id === nodeId);

            linkElements.classed("dimmed", l => {{
                const sId = typeof l.source === 'object' ? l.source.id : l.source;
                const tId = typeof l.target === 'object' ? l.target.id : l.target;
                return !(sId === nodeId || tId === nodeId);
            }}).classed("highlighted-link", l => {{
                const sId = typeof l.source === 'object' ? l.source.id : l.source;
                const tId = typeof l.target === 'object' ? l.target.id : l.target;
                return sId === nodeId || tId === nodeId;
            }});
        }}

        function clearHighlight() {{
            nodeElements.classed("dimmed", false).classed("highlighted-node", false);
            linkElements.classed("dimmed", false).classed("highlighted-link", false);
        }}

        // Click outside canvas clears selection
        svg.on("click", () => {{
            selectedNodeId = null;
            clearHighlight();
            document.getElementById("details-panel").innerHTML = `
                <div class="empty-details">
                    Click a node in the graph to view properties, contents, and connected relations.
                </div>
            `;
        }});

        // Filters Setup
        document.querySelectorAll(".filter-btn").forEach(btn => {{
            btn.addEventListener("click", () => {{
                const type = btn.getAttribute("data-type");
                activeFilters[type] = !activeFilters[type];
                btn.classList.toggle("active", activeFilters[type]);
                updateGraph();
            }});
        }});

        // Search Input Setup
        document.getElementById("search-input").addEventListener("input", event => {{
            searchQuery = event.target.value.toLowerCase().trim();
            updateGraph();
        }});

        // Label Toggle
        document.getElementById("btn-tag-toggle").addEventListener("click", () => {{
            showLabels = !showLabels;
            document.getElementById("btn-tag-toggle").textContent = showLabels ? "🏷️ Hide Labels" : "🏷️ Show Labels";
            updateGraph();
        }});

        // Tuning Sliders
        const slideCharge = document.getElementById("slide-charge");
        const valCharge = document.getElementById("val-charge");
        slideCharge.addEventListener("input", e => {{
            const val = parseInt(e.target.value);
            valCharge.textContent = val;
            sim.force("charge").strength(val);
            sim.alpha(0.2).restart();
        }});

        const slideDistance = document.getElementById("slide-distance");
        const valDistance = document.getElementById("val-distance");
        slideDistance.addEventListener("input", e => {{
            const val = parseInt(e.target.value);
            valDistance.textContent = val;
            sim.force("link").distance(d => {{
                if (d.relation === "sits_on") return val * 1.4;
                if (d.relation === "belongs_to") return val * 0.9;
                return val * 0.6;
            }});
            sim.alpha(0.2).restart();
        }});

        const slideCollision = document.getElementById("slide-collision");
        const valCollision = document.getElementById("val-collision");
        slideCollision.addEventListener("input", e => {{
            const val = parseInt(e.target.value);
            valCollision.textContent = val;
            sim.force("collision").radius(d => (sizeMap[d.type] || 10) + val);
            sim.alpha(0.2).restart();
        }});

        // Initial Build
        updateGraph();
        
        // Resize Handler
        window.addEventListener("resize", () => {{
            width = viewportEl.clientWidth;
            height = viewportEl.clientHeight;
            svg.attr("width", width).attr("height", height);
            sim.force("center", d3.forceCenter(width / 2, height / 2)).alpha(0.2).restart();
        }});
    </script>
</body>
</html>"""
    with open(path, "w", encoding="utf-8") as f:
        f.write(html)



def write_empty_fallback(path: str):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write("""<!DOCTYPE html>
<html>
<body style="background:#0a0812; color:#fff; font-family:sans-serif; text-align:center; padding-top:100px;">
    <h2>📚 No Library graph data yet.</h2>
    <p>The Hydragent dream cycle will populate Shelves, Books, and Pages as conversations are consolidated.</p>
</body>
</html>""")


if __name__ == "__main__":
    generate_graph()
