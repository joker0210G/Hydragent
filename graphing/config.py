from dataclasses import dataclass, field
from typing import Set, Dict

@dataclass
class Config:
    stopwords: Set[str] = field(default_factory=lambda: {
        "the", "and", "with", "for", "was", "were", "been", "have", "has", "had", 
        "this", "that", "user", "assistant", "greeted", "requested"
    })
    colors: Dict[str, str] = field(default_factory=lambda: {
        "shelf": "#a855f7",
        "book": "#3b82f6",
        "page": "#10b981",
        "tag": "#f59e0b"
    })
    radius: Dict[str, int] = field(default_factory=lambda: {
        "shelf": 14,
        "book": 10,
        "page": 7,
        "tag": 5
    })
    charge_strength: int = -200
    collision_padding: int = 12
    default_belongs_to_distance: int = 40
    default_sits_on_distance: int = 100
    default_has_tag_distance: int = 30
