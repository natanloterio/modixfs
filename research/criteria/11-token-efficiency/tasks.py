"""
Task definitions for the token-efficiency benchmark.

Each task is a dict with:
  - id:          short identifier used in Weave run names
  - description: human-readable label
  - user_turn:   the user message that kicks off the conversation
  - stop_when:   callable(assistant_text) -> bool — returns True when the
                 task is considered done (avoids open-ended loops)
"""

TASKS = [
    {
        "id": "list_users",
        "description": "Fetch and display all users (full format)",
        "user_turn": "List all users. Show their name and ID.",
        "stop_when": lambda text: any(
            kw in text.lower() for kw in ["user", "id:", "##", "jacobi", "rudolph"]
        ),
    },
    {
        "id": "single_user",
        "description": "Look up a single user by name",
        "user_turn": "Who is the user named 'Brad Jacobi'? Show their ID.",
        "stop_when": lambda text: (
            "jacobi" in text.lower()
            or "brad" in text.lower()
            or "not found" in text.lower()
            or any(c.isdigit() for c in text)
        ),
    },
    {
        "id": "count_users",
        "description": "Count how many users exist",
        "user_turn": "How many users are there in total?",
        "stop_when": lambda text: any(c.isdigit() for c in text),
    },
    # ── Multi-tool sequence task ──────────────────────────────────────────────
    {
        "id": "count_and_find",
        "description": "Multi-tool: count all users AND look up Brad Jacobi's ID (forces 2 separate tool calls)",
        "user_turn": "Two questions: (1) How many users are there in total? (2) What is Brad Jacobi's ID?",
        "stop_when": lambda text: (
            any(c.isdigit() for c in text)
            and ("jacobi" in text.lower() or "brad" in text.lower())
        ),
    },
    # ── Cross-reference task ──────────────────────────────────────────────────
    {
        "id": "cross_reference",
        "description": "Cross-reference: search by name to get ID, then fetch full record for createdAt (2-step lookup)",
        "user_turn": "When was Brad Jacobi's account created? Give me the exact date.",
        "stop_when": lambda text: (
            "2026" in text or "jacobi" in text.lower()
        ) and any(c.isdigit() for c in text),
    },
    # ── Aggregate + filter task ───────────────────────────────────────────────
    {
        "id": "filter_and_count",
        "description": "Aggregate+filter: fetch all users, filter by name criterion, count matches",
        "user_turn": "How many users have the letter 'a' in their name (case-insensitive)? List their names.",
        "stop_when": lambda text: (
            any(c.isdigit() for c in text)
            and any(kw in text.lower() for kw in ["user", "name", "total", "count", "found"])
        ),
    },
    # ── Composed-endpoint tasks (LF pipe vs MCP equivalent) ───────────────────
    {
        "id": "list_compact",
        "description": "List users in compact id:name format (tests pipe composition)",
        "user_turn": "List all users compactly — just their ID and name, one per line.",
        "stop_when": lambda text: any(c.isdigit() for c in text),
    },
    {
        "id": "count_composed",
        "description": "Count users via composed endpoint (tests pipe composition)",
        "user_turn": "How many users are there? Give me just the number.",
        "stop_when": lambda text: any(c.isdigit() for c in text),
    },
]
