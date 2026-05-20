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
        "description": "Fetch and display all users from the mock API",
        "user_turn": "List all users. Show their name and ID.",
        "stop_when": lambda text: any(
            kw in text.lower() for kw in ["user", "id:", "##"]
        ),
    },
    {
        "id": "single_user",
        "description": "Look up a single user by name",
        "user_turn": "Who is the user named 'Leanne Graham'? Show their ID.",
        "stop_when": lambda text: "leanne" in text.lower() or "not found" in text.lower(),
    },
    {
        "id": "count_users",
        "description": "Count how many users exist",
        "user_turn": "How many users are there in total?",
        "stop_when": lambda text: any(c.isdigit() for c in text),
    },
]
