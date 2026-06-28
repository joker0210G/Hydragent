import unittest
import sqlite3
import json
from graphing.config import Config
from graphing.models import Node, Edge
from graphing.builder import LibraryGraphBuilder

class TestGraphing(unittest.TestCase):
    def setUp(self):
        self.conn = sqlite3.connect(":memory:")
        self.cursor = self.conn.cursor()
        
        # Create mock tables matching the schema
        self.cursor.execute("""
            CREATE TABLE nodes (
                node_id TEXT PRIMARY KEY,
                type TEXT,
                label TEXT,
                properties TEXT
            )
        """)
        self.cursor.execute("""
            CREATE TABLE edges (
                edge_id TEXT PRIMARY KEY,
                source_node_id TEXT,
                target_node_id TEXT,
                relation_type TEXT,
                weight REAL
            )
        """)
        self.conn.commit()
        
        self.config = Config()
        self.builder = LibraryGraphBuilder(self.conn, self.config)

    def tearDown(self):
        self.conn.close()

    def test_stable_id(self):
        id1 = self.builder._stable_id("test", "parts")
        id2 = self.builder._stable_id("test", "parts")
        self.assertEqual(id1, id2)
        self.assertEqual(len(id1), 32) # Hex digest of 16-byte blake2b is 32 chars

    def test_generate_short_name_from_tags(self):
        page_tags = {"p1": ["rust", "coding"], "p2": ["rust"]}
        pages = [
            Node(id="p1", type="page", label="Page 1 description"),
            Node(id="p2", type="page", label="Page 2 description")
        ]
        name = self.builder._generate_short_name(["p1", "p2"], page_tags, pages)
        self.assertIn("Rust", name)

    def test_build_and_cluster_graph(self):
        # Insert mock pages
        self.cursor.execute("INSERT INTO nodes VALUES ('p1', 'page', 'Greeting conversation', '{\"source\": \"dream\"}')")
        self.cursor.execute("INSERT INTO nodes VALUES ('p2', 'page', 'Another greeting', '{\"source\": \"dream\"}')")
        # Insert mock tag edges
        self.cursor.execute("INSERT INTO edges VALUES ('e1', 'p1', '__tag__:greeting', 'tag', 1.0)")
        self.cursor.execute("INSERT INTO edges VALUES ('e2', 'p2', '__tag__:greeting', 'tag', 1.0)")
        self.conn.commit()

        self.builder.build_and_cluster_graph()

        # Verify books were created
        self.cursor.execute("SELECT node_id, type, label FROM nodes WHERE type = 'book'")
        books = self.cursor.fetchall()
        self.assertEqual(len(books), 1)
        self.assertEqual(books[0][2], "Greeting")

        # Verify belongs_to edges
        self.cursor.execute("SELECT relation_type FROM edges WHERE relation_type = 'belongs_to'")
        edges = self.cursor.fetchall()
        self.assertEqual(len(edges), 2)

if __name__ == "__main__":
    unittest.main()
