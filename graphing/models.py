from dataclasses import dataclass, field
from typing import Optional, Any, Dict

@dataclass
class Node:
    id: str
    type: str
    label: str
    properties: Dict[str, Any] = field(default_factory=dict)

@dataclass
class Edge:
    id: str
    source: str
    target: str
    relation: str
    weight: float = 1.0
