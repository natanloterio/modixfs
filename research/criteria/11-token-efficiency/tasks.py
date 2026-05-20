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
