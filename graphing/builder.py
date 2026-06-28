import sqlite3
import hashlib
import json
import logging
import time
from typing import List, Dict, Set, Tuple
from .models import Node, Edge
from .data_access import NodeDAO, EdgeDAO
from .config import Config

logger = logging.getLogger("hydragent.graph")

def timed(func):
    """Decorator to measure and log execution time of functions."""
    def wrapper(*args, **kwargs):
        start = time.time()
        result = func(*args, **kwargs)
        duration = (time.time() - start) * 1000
        logger.info("Function %s took %.2f ms", func.__name__, duration)
        return result
    return wrapper

class LibraryGraphBuilder:
    def __init__(self, conn: sqlite3.Connection, config: Config):
        self.conn = conn
        self.config = config
        self.node_dao = NodeDAO(conn)
        self.edge_dao = EdgeDAO(conn)

    def _stable_id(self, *parts: str) -> str:
        """Generate a stable, deterministic node ID from string parts using BLAKE2b."""
        return hashlib.blake2b("-".join(parts).encode(), digest_size=16).hexdigest()

    def _generate_short_name(self, page_ids: List[str], page_tags: Dict[str, List[str]], pages: List[Node], suggested_key: str = "suggested_books") -> str:
        """Generate a 1-2 word short name for a cluster, prioritizing LLM suggested names, then page tags and keywords."""
        # 1. Prioritize LLM suggested names (semantic hints)
        suggestions = []
        for pid in page_ids:
            for p in pages:
                if p.id == pid:
                    hints = p.properties.get(suggested_key) or []
                    suggestions.extend([h for h in hints if h])
        
        if suggestions:
            from collections import Counter
            most_common = Counter(suggestions).most_common(1)
            if most_common:
                return most_common[0][0].strip().title()

        # 2. Fall back to tags
        tags = []
        labels = []
        for pid in page_ids:
            if pid in page_tags:
                tags.extend(page_tags[pid])
            for p in pages:
                if p.id == pid:
                    labels.append(p.label)

        if tags:
            from collections import Counter
            most_common = Counter(tags).most_common(2)
            if most_common:
                return " & ".join([t.title() for t, _ in most_common])

        import re
        words = []
        for label in labels:
            words.extend([w.lower() for w in re.findall(r'\b[a-zA-Z]{4,}\b', label) if w.lower() not in self.config.stopwords])
        
        if words:
            from collections import Counter
            most_common = Counter(words).most_common(2)
            if most_common:
                return " & ".join([w.title() for w, _ in most_common])
                
        return "General Topic"

    @timed
    def build_and_cluster_graph(self):
        pages = self.node_dao.load_nodes_by_type("page")
        if not pages:
            logger.info("No pages found in database; skipping graph generation.")
            return

        page_ids = [p.id for p in pages]
        page_tags = self.edge_dao.load_page_tags_batch(page_ids)

        # 1. Cluster Pages into Books
        communities = self._cluster_pages(pages, page_tags)

        # Merge single-page communities with no tags into a General book
        general_pages = []
        filtered_communities = {}
        for cid, pids in communities.items():
            if len(pids) == 1:
                if not page_tags.get(pids[0]):
                    general_pages.extend(pids)
                    continue
            filtered_communities[cid] = pids

        if general_pages:
            filtered_communities["__GENERAL__"] = general_pages

        new_books: List[Node] = []
        new_belongs_to_edges: List[Edge] = []
        book_metadata: List[Dict] = []

        for cid, pids in filtered_communities.items():
            if cid == "__GENERAL__":
                book_label = "General Conversations"
            else:
                book_label = self._generate_short_name(pids, page_tags, pages)
            
            book_id = f"book-{self._stable_id(str(cid), *sorted(pids))}"
            new_books.append(Node(
                id=book_id,
                type="book",
                label=book_label,
                properties={"source": "graphify_cluster", "page_count": len(pids), "community_id": cid}
            ))
            book_metadata.append({"id": book_id, "label": book_label, "page_ids": pids})

            for pid in pids:
                new_belongs_to_edges.append(Edge(
                    id=f"{pid}-belongs_to-{book_id}",
                    source=pid,
                    target=book_id,
                    relation="belongs_to"
                ))

        # 2. Cluster Books into Shelves
        shelf_communities = self._cluster_books(book_metadata)
        
        general_books = []
        filtered_shelves = {}
        for cid, bids in shelf_communities.items():
            if len(bids) == 1:
                general_books.extend(bids)
                continue
            filtered_shelves[cid] = bids

        if general_books:
            filtered_shelves["__GENERAL__"] = general_books

        new_shelves: List[Node] = []
        new_sits_on_edges: List[Edge] = []

        for cid, bids in filtered_shelves.items():
            if cid == "__GENERAL__":
                shelf_label = "General Archive"
            else:
                all_pids = []
                for bid in bids:
                    book = next((b for b in book_metadata if b["id"] == bid), None)
                    if book:
                        all_pids.extend(book["page_ids"])
                shelf_label = self._generate_short_name(all_pids, page_tags, pages, suggested_key="suggested_shelves")

            shelf_id = f"shelf-{self._stable_id(str(cid), *sorted(bids))}"
            new_shelves.append(Node(
                id=shelf_id,
                type="shelf",
                label=shelf_label,
                properties={"source": "graphify_cluster", "book_count": len(bids), "community_id": cid}
            ))

            for bid in bids:
                new_sits_on_edges.append(Edge(
                    id=f"{bid}-sits_on-{shelf_id}",
                    source=bid,
                    target=shelf_id,
                    relation="sits_on"
                ))

        # 3. Incremental Database Writes (Diffing)
        self._sync_nodes_and_edges(new_books + new_shelves, new_belongs_to_edges + new_sits_on_edges)
        self.conn.commit()

    def _sync_nodes_and_edges(self, target_nodes: List[Node], target_edges: List[Edge]):
        """Compare target state with database state and apply minimal diff writes."""
        # Load existing graphify nodes/edges
        existing_nodes = {n.id: n for n in self.node_dao.load_all_nodes() if n.properties.get("source") == "graphify_cluster"}
        existing_edges = {e.id: e for e in self.edge_dao.load_all_edges() if e.id.startswith("book-") or "-sits_on-shelf-" in e.id or "-belongs_to-book-" in e.id}

        # Diff Nodes
        target_node_ids = {n.id for n in target_nodes}
        for n in target_nodes:
            if n.id not in existing_nodes or existing_nodes[n.id].label != n.label:
                self.node_dao.write_node(n)
        for nid in existing_nodes:
            if nid not in target_node_ids:
                cursor = self.conn.cursor()
                cursor.execute("DELETE FROM nodes WHERE node_id = ?", (nid,))

        # Diff Edges
        target_edge_ids = {e.id for e in target_edges}
        for e in target_edges:
            if e.id not in existing_edges:
                self.edge_dao.write_edge(e)
        for eid in existing_edges:
            if eid not in target_edge_ids:
                cursor = self.conn.cursor()
                cursor.execute("DELETE FROM edges WHERE edge_id = ?", (eid,))

    @timed
    def _cluster_pages(self, pages: List[Node], page_tags: Dict[str, List[str]]) -> Dict[str, List[str]]:
        try:
            from graphify.cluster import cluster as graphify_cluster
            import networkx as nx
            
            G = nx.Graph()
            for p in pages:
                G.add_node(p.id)
            
            # Add edges between pages sharing tags or LLM suggested books/shelves
            for i, p_a in enumerate(pages):
                suggested_books_a = set(b.lower() for b in p_a.properties.get("suggested_books") or [] if b)
                suggested_shelves_a = set(s.lower() for s in p_a.properties.get("suggested_shelves") or [] if s)
                
                for p_b in pages[i+1:]:
                    weight = 0.0
                    # 1. Share tags (standard Graphify)
                    shared_tags = set(page_tags.get(p_a.id, [])) & set(page_tags.get(p_b.id, []))
                    if shared_tags:
                        weight += len(shared_tags) * 1.0
                        
                    # 2. Share LLM suggested books (high weight semantic hint)
                    suggested_books_b = set(b.lower() for b in p_b.properties.get("suggested_books") or [] if b)
                    shared_books = suggested_books_a & suggested_books_b
                    if shared_books:
                        weight += len(shared_books) * 5.0
                        
                    # 3. Share LLM suggested shelves (medium weight semantic hint)
                    suggested_shelves_b = set(s.lower() for s in p_b.properties.get("suggested_shelves") or [] if s)
                    shared_shelves = suggested_shelves_a & suggested_shelves_b
                    if shared_shelves:
                        weight += len(shared_shelves) * 2.0
                        
                    if weight > 0.0:
                        G.add_edge(p_a.id, p_b.id, weight=weight)
            
            communities = graphify_cluster(G)
            if isinstance(communities, dict):
                return {str(k): list(v) for k, v in communities.items()}
            elif isinstance(communities, list):
                # Normalize if it's a list of sets
                return {str(i): list(c) for i, c in enumerate(communities)}
        except ImportError:
            logger.warning("graphify or networkx not available; using fallback keyword clustering.")
            
        # Fallback Greedy Clustering
        communities = {}
        assigned = set()
        cid = 0
        for p in pages:
            if p.id in assigned:
                continue
            cid += 1
            cluster = [p.id]
            assigned.add(p.id)
            kws = set(page_tags.get(p.id, []))
            suggested_books = set(b.lower() for b in p.properties.get("suggested_books") or [] if b)
            
            for other in pages:
                if other.id in assigned:
                    continue
                other_kws = set(page_tags.get(other.id, []))
                other_books = set(b.lower() for b in other.properties.get("suggested_books") or [] if b)
                
                if (kws & other_kws) or (suggested_books & other_books):
                    cluster.append(other.id)
                    assigned.add(other.id)
                    kws |= other_kws
                    suggested_books |= other_books
            communities[str(cid)] = cluster
        return communities

    @timed
    def _cluster_books(self, book_metadata: List[Dict]) -> Dict[str, List[str]]:
        # Default simple grouping for Books -> Shelves
        return {"1": [b["id"] for b in book_metadata]}
