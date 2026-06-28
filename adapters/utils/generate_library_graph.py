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
regenerate the interactive D3.js visualisation.
"""

import os
import sys
import sqlite3
import json
import uuid
import hashlib

# Inject paths so we can run this script from anywhere
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..")))

# ── Graphify local clustering (75% weight) ─────────────────────────────────
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
        return os.path.abspath(os.path.join(os.environ["HYDRAGENT_HOME"], "data", "sessions.db"))
    
    # 2. Check standard Windows/Unix home directory path
    home = os.path.expanduser("~")
    return os.path.abspath(os.path.join(home, ".hydragent", "data", "sessions.db"))


def _get_output_html():
    # 1. Check explicit environment override
    if "HYDRAGENT_HOME" in os.environ:
        return os.path.abspath(os.path.join(os.environ["HYDRAGENT_HOME"], "data", "graph.html"))
    
    # 2. Check standard Windows/Unix home directory path
    home = os.path.expanduser("~")
    return os.path.abspath(os.path.join(home, ".hydragent", "data", "graph.html"))


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
    Load tags associated with a page from the edges table.
    """
    cursor = conn.cursor()
    cursor.execute(
        """SELECT target_node_id FROM edges 
           WHERE source_node_id = ? AND relation_type = 'tag'""",
        (page_id,)
    )
    return [row[0].replace("__tag__:", "") for row in cursor.fetchall()]


def _generate_short_name(conn, page_ids: list[str]) -> str:
    """Generate a 1-2 word short name for a cluster based on page tags and keywords."""
    cursor = conn.cursor()
    tags = []
    labels = []
    for pid in page_ids:
        cursor.execute("SELECT target_node_id FROM edges WHERE source_node_id = ? AND relation_type = 'tag'", (pid,))
        tags.extend([row[0].replace("__tag__:", "") for row in cursor.fetchall()])
        cursor.execute("SELECT label FROM nodes WHERE node_id = ?", (pid,))
        row = cursor.fetchone()
        if row:
            labels.append(row[0])

    if tags:
        from collections import Counter
        most_common = Counter(tags).most_common(2)
        if most_common:
            return " & ".join([t.title() for t, _ in most_common])

    import re
    STOPWORDS = {"the", "and", "with", "for", "was", "were", "been", "have", "has", "had", "this", "that", "user", "assistant", "greeted", "requested"}
    words = []
    for label in labels:
        words.extend([w.lower() for w in re.findall(r'\b[a-zA-Z]{4,}\b', label) if w.lower() not in STOPWORDS])
    
    if words:
        from collections import Counter
        most_common = Counter(words).most_common(2)
        if most_common:
            return " & ".join([w.title() for w, _ in most_common])
            
    return "General Topic"



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

    # ── Step 0: Clean up stale graphify nodes and edges ─────────────────────
    cursor.execute("DELETE FROM edges WHERE edge_id LIKE 'book-%' OR edge_id LIKE '%-sits_on-shelf-%'")
    cursor.execute("DELETE FROM nodes WHERE type IN ('book', 'shelf') AND properties LIKE '%\"source\": \"graphify_cluster\"%'")
    conn.commit()

    # ── Step 1: Load Page nodes (written by dream cycle LLM step) ───────────
    pages = load_page_nodes(conn)
    print(f"[generate_library_graph] Loaded {len(pages)} Page nodes from Library")

    book_nodes_created: list[dict] = []
    if GRAPHIFY_AVAILABLE and pages:
        print("[generate_library_graph] Running Leiden/Louvain clustering: Pages -> Books")
        raw_communities = cluster_pages_into_books(conn, pages)

        # Merge single-page communities that have no distinct tags into a "General" book
        communities = {}
        general_pages = []
        for cid, page_ids in raw_communities.items():
            if len(page_ids) == 1:
                p_tags = load_page_tags(conn, page_ids[0])
                if not p_tags:
                    general_pages.extend(page_ids)
                    continue
            communities[cid] = page_ids
            
        if general_pages:
            communities["general"] = general_pages

        for cid, page_ids in communities.items():
            if not page_ids:
                continue
            
            if cid == "general":
                book_label = "General Conversations"
            else:
                book_label = _generate_short_name(conn, page_ids)
                
            book_id = f"book-{_stable_id(str(cid), *sorted(page_ids))}"

            write_node(conn, book_id, "book", book_label, {
                "community_id": cid,
                "page_count": len(page_ids),
                "source": "graphify_cluster",
            })
            book_nodes_created.append({"id": book_id, "label": book_label, "page_ids": page_ids})

            # Write belongs_to edges: Page -> Book
            for pid in page_ids:
                write_edge(conn, pid, book_id, "belongs_to", weight=1.0)

        print(f"[generate_library_graph] Created {len(book_nodes_created)} Book nodes")

    # ── Step 3: Louvain clustering -> Books into Shelves ─────────────────────
    if GRAPHIFY_AVAILABLE and book_nodes_created:
        print("[generate_library_graph] Running Leiden/Louvain clustering: Books -> Shelves")
        raw_shelf_communities = cluster_books_into_shelves(conn, book_nodes_created)

        # Merge single-book shelves into a "General Archive" shelf
        shelf_communities = {}
        general_books = []
        for cid, book_ids in raw_shelf_communities.items():
            if len(book_ids) == 1:
                general_books.extend(book_ids)
                continue
            shelf_communities[cid] = book_ids
            
        if general_books:
            shelf_communities["general"] = general_books

        shelf_count = 0
        for cid, book_ids in shelf_communities.items():
            if not book_ids:
                continue
                
            if cid == "general":
                shelf_label = "General Archive"
            else:
                # Name the Shelf by compiling the page IDs of all books sitting on it
                all_page_ids = []
                for bid in book_ids:
                    book_node = next((b for b in book_nodes_created if b["id"] == bid), None)
                    if book_node:
                        all_page_ids.extend(book_node["page_ids"])
                shelf_label = _generate_short_name(conn, all_page_ids)
                
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
        
        # Skip database-created books and shelves that aren't part of the graphify clustering
        if row["type"] in ("book", "shelf"):
            if not props or props.get("source") != "graphify_cluster":
                continue
                
        all_nodes.append({
            "id": row["node_id"],
            "type": row["type"],
            "label": row["label"],
            "properties": props,
        })

    node_ids = {n["id"] for n in all_nodes}
    cursor.execute(
        "SELECT edge_id, source_node_id, target_node_id, relation_type, weight FROM edges"
    )
    all_links = []
    for row in cursor.fetchall():
        s_id = row["source_node_id"]
        t_id = row["target_node_id"]
        # Only include links where both source and target exist in our filtered nodes
        if s_id in node_ids and t_id in node_ids:
            all_links.append({
                "id": row["edge_id"],
                "source": s_id,
                "target": t_id,
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
            font-size: 24px;
            font-weight: 700;
            color: #fff;
            line-height: 1.2;
        }}
        
        .detail-meta {{
            font-size: 12px;
            color: #8c8aa2;
            display: flex;
            flex-direction: column;
            gap: 6px;
            padding: 16px;
            background: rgba(255, 255, 255, 0.02);
            border: 1px solid rgba(255, 255, 255, 0.05);
            border-radius: 8px;
        }}
        
        .meta-row {{
            display: flex;
            justify-content: space-between;
        }}
        
        .meta-label {{
            font-weight: 500;
        }}
        
        .meta-value {{
            color: #fff;
        }}
        
        .detail-section-title {{
            font-size: 14px;
            font-weight: 600;
            color: #a855f7;
            text-transform: uppercase;
            letter-spacing: 0.5px;
            margin-top: 8px;
        }}
        
        .tags-list {{
            display: flex;
            flex-wrap: wrap;
            gap: 6px;
        }}
        
        .tag-pill {{
            padding: 4px 10px;
            background: rgba(255, 255, 255, 0.05);
            border: 1px solid rgba(255, 255, 255, 0.08);
            border-radius: 4px;
            font-size: 11px;
            color: #c1bfd6;
        }}
        
        .connections-list {{
            display: flex;
            flex-direction: column;
            gap: 8px;
        }}
        
        .connection-item {{
            display: flex;
            justify-content: space-between;
            align-items: center;
            padding: 10px 12px;
            background: rgba(255, 255, 255, 0.03);
            border: 1px solid rgba(255, 255, 255, 0.05);
            border-radius: 6px;
            font-size: 13px;
            cursor: pointer;
            transition: all 0.2s ease;
        }}
        
        .connection-item:hover {{
            background: rgba(255, 255, 255, 0.06);
            border-color: rgba(255, 255, 255, 0.1);
        }}
        
        .conn-label {{
            font-weight: 500;
            color: #fff;
            white-space: nowrap;
            overflow: hidden;
            text-overflow: ellipsis;
            max-width: 180px;
        }}
        
        .conn-relation {{
            font-size: 10px;
            color: #8c8aa2;
            text-transform: uppercase;
            letter-spacing: 0.5px;
            padding: 2px 6px;
            background: rgba(255, 255, 255, 0.05);
            border-radius: 4px;
        }}
        
        /* Toggle Sidebar Button */
        #toggle-sidebar {{
            position: absolute;
            left: 396px;
            top: 24px;
            width: 40px;
            height: 40px;
            background: rgba(13, 11, 23, 0.8);
            border: 1px solid rgba(255, 255, 255, 0.1);
            border-radius: 50%;
            cursor: pointer;
            z-index: 9;
            display: flex;
            align-items: center;
            justify-content: center;
            color: #fff;
            backdrop-filter: blur(10px);
            box-shadow: 0 4px 20px rgba(0, 0, 0, 0.3);
            transition: all 0.3s cubic-bezier(0.4, 0, 0.2, 1);
        }}
        
        #toggle-sidebar.collapsed {{
            left: 24px;
        }}
        
        #toggle-sidebar:hover {{
            background: rgba(168, 85, 247, 0.2);
            border-color: rgba(168, 85, 247, 0.4);
            box-shadow: 0 0 15px rgba(168, 85, 247, 0.3);
        }}
        
        #toggle-sidebar svg {{
            width: 20px;
            height: 20px;
            transition: transform 0.3s ease;
        }}
        
        #toggle-sidebar.collapsed svg {{
            transform: rotate(180deg);
        }}
        
        /* SVG Graph Container */
        #graph-container {{
            width: 100vw;
            height: 100vh;
            z-index: 1;
        }}
        
        /* D3 Elements Styling */
        .node {{
            cursor: pointer;
            stroke-width: 1.5px;
            transition: r 0.2s ease, stroke-width 0.2s ease;
        }}
        
        .node:hover {{
            stroke-width: 2.5px;
        }}
        
        .link {{
            stroke-opacity: 0.15;
            stroke-dasharray: none;
            transition: stroke-opacity 0.2s ease, stroke-width 0.2s ease;
        }}
        
        .link.active {{
            stroke-opacity: 0.8 !important;
            stroke-width: 2.5px !important;
        }}
        
        .node-label {{
            font-size: 11px;
            font-weight: 500;
            pointer-events: none;
            fill: #c1bfd6;
            text-shadow: 0 2px 4px rgba(0,0,0,0.8), 0 -2px 4px rgba(0,0,0,0.8), 2px 0 4px rgba(0,0,0,0.8), -2px 0 4px rgba(0,0,0,0.8);
        }}
        
        .node.active-focus {{
            stroke: #fff !important;
            stroke-width: 3px !important;
            filter: drop-shadow(0 0 8px currentColor);
        }}
        
        /* Mini App / Telegram specific controls */
        #header-actions {{
            position: absolute;
            right: 24px;
            top: 24px;
            display: flex;
            gap: 8px;
            z-index: 8;
        }}
        
        .action-btn {{
            padding: 10px 16px;
            background: rgba(13, 11, 23, 0.8);
            border: 1px solid rgba(255, 255, 255, 0.1);
            border-radius: 8px;
            color: #c1bfd6;
            font-family: inherit;
            font-size: 13px;
            font-weight: 600;
            cursor: pointer;
            backdrop-filter: blur(10px);
            display: flex;
            align-items: center;
            gap: 8px;
            transition: all 0.2s ease;
            box-shadow: 0 4px 20px rgba(0, 0, 0, 0.3);
        }}
        
        .action-btn:hover {{
            background: rgba(255, 255, 255, 0.05);
            color: #fff;
            border-color: rgba(255, 255, 255, 0.2);
        }}
        
        .action-btn.primary {{
            background: linear-gradient(135deg, #a855f7, #3b82f6);
            border: none;
            color: #fff;
        }}
        
        .action-btn.primary:hover {{
            opacity: 0.9;
            box-shadow: 0 0 15px rgba(168, 85, 247, 0.4);
        }}
    </style>
</head>
<body>

    <!-- Toggle Sidebar Button -->
    <button id="toggle-sidebar" title="Toggle Sidebar">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <polyline points="15 18 9 12 15 6"></polyline>
        </svg>
    </button>

    <!-- Sidebar Panel -->
    <div id="sidebar">
        <div class="sidebar-header">
            <h1 class="sidebar-title">
                <span style="font-size: 24px;">🐉</span> Hydragent Library
            </h1>
            <div class="sidebar-subtitle">Knowledge Graph Explorer</div>
        </div>
        
        <div class="search-container">
            <input type="text" id="search-input" class="search-box" placeholder="Search nodes or tags...">
        </div>
        
        <div class="filter-section">
            <h2 class="section-title">Filter Node Types</h2>
            <div class="filter-grid">
                <button class="filter-btn active" data-type="shelf">
                    <span style="color: #a855f7;">●</span> Shelves
                </button>
                <button class="filter-btn active" data-type="book">
                    <span style="color: #3b82f6;">●</span> Books
                </button>
                <button class="filter-btn active" data-type="page">
                    <span style="color: #10b981;">●</span> Pages
                </button>
                <button class="filter-btn active" data-type="tag">
                    <span style="color: #f59e0b;">●</span> Tags
                </button>
            </div>
        </div>
        
        <div class="details-container" id="details-panel">
            <div class="empty-details">
                Select a node in the graph to view its connections and properties.
            </div>
        </div>
    </div>

    <!-- Top Right Header Actions -->
    <div id="header-actions">
        <button class="action-btn" id="btn-reset">
            <svg style="width: 16px; height: 16px;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"></path>
            </svg>
            Reset View
        </button>
        <button class="action-btn primary" id="btn-sync">
            <svg style="width: 16px; height: 16px;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"></path>
            </svg>
            Re-Cluster Graph
        </button>
    </div>

    <!-- Graph Container -->
    <div id="graph-container"></div>

    <script>
        // Graph data injected by Python pipeline
        const graphData = {{
            nodes: {json.dumps(nodes)},
            links: {json.dumps(links)}
        }};
    </script>
    <script>
        // --- D3 Graph Implementation ---
        const width = window.innerWidth;
        const height = window.innerHeight;
        
        // Color Palette
        const colors = {{
            shelf: "#a855f7",
            book: "#3b82f6",
            page: "#10b981",
            tag: "#f59e0b"
        }};
        
        // Node radius sizing
        const radius = {{
            shelf: 14,
            book: 10,
            page: 7,
            tag: 5
        }};

        // Process Tags as virtual nodes to show them on the graph
        const processedNodes = [...graphData.nodes];
        const processedLinks = [...graphData.links];
        
        // Extract tags from page properties and create virtual tag nodes & links
        const tagSet = new Set();
        graphData.nodes.forEach(node => {{
            if (node.type === 'page' && node.properties && node.properties.tags) {{
                node.properties.tags.forEach(tag => {{
                    tagSet.add(tag);
                    processedLinks.push({{
                        id: `${{node.id}}-has_tag-${{tag}}`,
                        source: node.id,
                        target: `tag-${{tag}}`,
                        relation: "has_tag",
                        weight: 0.5
                    }});
                }});
            }}
        }});
        
        tagSet.forEach(tag => {{
            processedNodes.push({{
                id: `tag-${{tag}}`,
                type: "tag",
                label: `#${{tag}}`,
                properties: {{}}
            }});
        }});

        // Setup SVG
        const svg = d3.select("#graph-container")
            .append("svg")
            .attr("width", "100%")
            .attr("height", "100%")
            .attr("viewBox", [0, 0, width, height]);
            
        const g = svg.append("g");

        // Zoom Behavior
        const zoom = d3.zoom()
            .scaleExtent([0.1, 8])
            .on("zoom", (event) => {{
                g.attr("transform", event.transform);
            }});
            
        svg.call(zoom);

        // Simulation Setup
        const simulation = d3.forceSimulation(processedNodes)
            .force("link", d3.forceLink(processedLinks).id(d => d.id).distance(d => {{
                if (d.relation === 'belongs_to') return 40;
                if (d.relation === 'sits_on') return 100;
                if (d.relation === 'has_tag') return 30;
                return 60;
            }}))
            .force("charge", d3.forceManyBody().strength(-200))
            .force("center", d3.forceCenter(width / 2 + 100, height / 2))
            .force("collision", d3.forceCollide().radius(d => radius[d.type] + 12));

        // Render Links
        const link = g.append("g")
            .attr("stroke", "#2d2a45")
            .selectAll("line")
            .data(processedLinks)
            .join("line")
            .attr("class", "link")
            .attr("stroke-width", d => d.relation === 'belongs_to' ? 2 : 1);

        // Render Nodes
        const node = g.append("g")
            .selectAll("circle")
            .data(processedNodes)
            .join("circle")
            .attr("class", "node")
            .attr("r", d => radius[d.type])
            .attr("fill", d => colors[d.type])
            .attr("stroke", "#08070e")
            .attr("stroke-width", 1.5)
            .call(drag(simulation));
            
        // Render Labels
        const label = g.append("g")
            .selectAll("text")
            .data(processedNodes)
            .join("text")
            .attr("class", "node-label")
            .attr("dy", d => -radius[d.type] - 4)
            .attr("text-anchor", "middle")
            .text(d => d.label.length > 20 ? d.label.substring(0, 20) + "..." : d.label);

        // Simulation Tick
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

        // Drag Behavior
        function drag(simulation) {{
            return d3.drag()
                .on("start", (event, d) => {{
                    if (!event.active) simulation.alphaTarget(0.3).restart();
                    d.fx = d.x;
                    d.fy = d.y;
                }})
                .on("drag", (event, d) => {{
                    d.fx = event.x;
                    d.fy = event.y;
                }})
                .on("end", (event, d) => {{
                    if (!event.active) simulation.alphaTarget(0);
                    d.fx = null;
                    d.fy = null;
                }});
        }}

        // --- Interactive Features ---
        let selectedNode = null;
        
        // Node Click Event
        node.on("click", (event, d) => {{
            event.stopPropagation();
            selectNode(d);
        }});
        
        svg.on("click", () => {{
            clearSelection();
        }});
        
        function selectNode(d) {{
            selectedNode = d;
            
            // Visual Updates
            node.classed("active-focus", n => n.id === d.id);
            
            // Highlight connections
            const connectedNodeIds = new Set([d.id]);
            link.classed("active", l => {{
                const isConnected = l.source.id === d.id || l.target.id === d.id;
                if (isConnected) {{
                    connectedNodeIds.add(l.source.id);
                    connectedNodeIds.add(l.target.id);
                }}
                return isConnected;
            }});
            
            // Fade unconnected nodes
            node.style("opacity", n => connectedNodeIds.has(n.id) ? 1.0 : 0.25);
            label.style("opacity", n => connectedNodeIds.has(n.id) ? 1.0 : 0.1);
            link.style("stroke-opacity", l => l.source.id === d.id || l.target.id === d.id ? 0.8 : 0.05);

            // Populate Sidebar
            populateSidebar(d);
            
            // Center View on selected node
            const transform = d3.zoomTransform(svg.node());
            svg.transition().duration(750).call(
                zoom.transform,
                d3.zoomIdentity.translate(width/2 + 100 - d.x * transform.k, height/2 - d.y * transform.k).scale(transform.k)
            );
        }}
        
        function clearSelection() {{
            selectedNode = null;
            node.classed("active-focus", false).style("opacity", 1.0);
            label.style("opacity", 1.0);
            link.classed("active", false).style("stroke-opacity", 0.15);
            
            d3.select("#details-panel").html(`
                <div class="empty-details">
                    Select a node in the graph to view its connections and properties.
                </div>
            `);
        }}

        function populateSidebar(d) {{
            const panel = d3.select("#details-panel");
            panel.html(""); // Clear
            
            const wrapper = panel.append("div").attr("class", "node-details");
            
            // Badge
            wrapper.append("div")
                .attr("class", `detail-badge badge-${{d.type}}`)
                .text(d.type);
                
            // Title
            wrapper.append("h2")
                .attr("class", "detail-title")
                .text(d.label);
                
            // Meta Section
            const meta = wrapper.append("div").attr("class", "detail-meta");
            meta.append("div").attr("class", "meta-row").html(`<span class="meta-label">ID:</span><span class="meta-value">${{d.id}}</span>`);
            
            if (d.type === 'page') {{
                meta.append("div").attr("class", "meta-row").html(`<span class="meta-label">Consolidated:</span><span class="meta-value">${{d.properties.consolidated ? 'Yes' : 'No'}}</span>`);
                if (d.properties.turn_count) {{
                    meta.append("div").attr("class", "meta-row").html(`<span class="meta-label">Turn Count:</span><span class="meta-value">${{d.properties.turn_count}}</span>`);
                }}
            }} else if (d.type === 'book') {{
                meta.append("div").attr("class", "meta-row").html(`<span class="meta-label">Page Count:</span><span class="meta-value">${{d.properties.page_count || 0}}</span>`);
            }} else if (d.type === 'shelf') {{
                meta.append("div").attr("class", "meta-row").html(`<span class="meta-label">Book Count:</span><span class="meta-value">${{d.properties.book_count || 0}}</span>`);
            }}
            
            // Tags
            if (d.type === 'page' && d.properties.tags && d.properties.tags.length > 0) {{
                wrapper.append("h3").attr("class", "detail-section-title").text("Associated Tags");
                const tagWrapper = wrapper.append("div").attr("class", "tags-list");
                d.properties.tags.forEach(tag => {{
                    tagWrapper.append("span").attr("class", "tag-pill").text(`#${{tag}}`);
                }});
            }}
            
            // Connections
            const connections = processedLinks.filter(l => l.source.id === d.id || l.target.id === d.id);
            if (connections.length > 0) {{
                wrapper.append("h3").attr("class", "detail-section-title").text("Graph Connections");
                const connWrapper = wrapper.append("div").attr("class", "connections-list");
                
                connections.forEach(l => {{
                    const targetNode = l.source.id === d.id ? l.target : l.source;
                    const item = connWrapper.append("div")
                        .attr("class", "connection-item")
                        .on("click", () => selectNode(targetNode));
                        
                    item.append("span").attr("class", "conn-label").text(targetNode.label);
                    item.append("span").attr("class", "conn-relation").text(l.relation);
                }});
            }}
        }}

        // --- UI Event Listeners ---
        // Toggle Sidebar
        const sidebar = d3.select("#sidebar");
        const toggleBtn = d3.select("#toggle-sidebar");
        toggleBtn.on("click", () => {{
            const collapsed = sidebar.classed("collapsed");
            sidebar.classed("collapsed", !collapsed);
            toggleBtn.classed("collapsed", !collapsed);
        }});
        
        // Reset Zoom & Pan
        d3.select("#btn-reset").on("click", () => {{
            svg.transition().duration(750).call(
                zoom.transform,
                d3.zoomIdentity
            );
        }});
        
        // Re-Cluster Trigger (Telegram WebApp / RPC bridge integration)
        d3.select("#btn-sync").on("click", () => {{
            const btn = d3.select("#btn-sync");
            btn.text("Clustering...").attr("disabled", true);
            
            // Send WebSocket request or trigger RPC if inside Telegram Mini App
            if (window.Telegram && window.Telegram.WebApp) {{
                window.Telegram.WebApp.sendData(JSON.stringify({{action: "recluster"}}));
            }} else {{
                // Fallback local API request
                fetch("/ws", {{ method: "POST", body: JSON.stringify({{ method: "library.recluster" }}) }})
                    .then(() => setTimeout(() => window.location.reload(), 1500))
                    .catch(() => setTimeout(() => window.location.reload(), 1500));
            }}
        }});
        
        // Search Filter
        d3.select("#search-input").on("input", (e) => {{
            const query = e.target.value.toLowerCase().trim();
            if (!query) {{
                node.style("opacity", 1.0);
                label.style("opacity", 1.0);
                return;
            }}
            
            node.style("opacity", n => 
                n.label.toLowerCase().includes(query) || 
                (n.type === 'page' && n.properties.tags && n.properties.tags.some(t => t.toLowerCase().includes(query))) 
                ? 1.0 : 0.15
            );
            label.style("opacity", n => n.label.toLowerCase().includes(query) ? 1.0 : 0.05);
        }});

        // Type Filters
        const activeFilters = new Set(["shelf", "book", "page", "tag"]);
        d3.selectAll(".filter-btn").on("click", function() {{
            const btn = d3.select(this);
            const type = btn.attr("data-type");
            
            if (activeFilters.has(type)) {{
                activeFilters.delete(type);
                btn.classed("active", false);
            }} else {{
                activeFilters.add(type);
                btn.classed("active", true);
            }}
            
            // Apply Filters
            node.style("display", n => activeFilters.has(n.type) ? "block" : "none");
            label.style("display", n => activeFilters.has(n.type) ? "block" : "none");
            link.style("display", l => activeFilters.has(l.source.type) && activeFilters.has(l.target.type) ? "block" : "none");
            
            simulation.alpha(0.3).restart();
        }});
    </script>
</body>
</html>
"""
    with open(path, "w", encoding="utf-8") as f:
        f.write(html)


# ── Empty Fallback ────────────────────────────────────────────────────────

def write_empty_fallback(path: str):
    _write_d3_html(path, [], [])


if __name__ == "__main__":
    generate_graph()
