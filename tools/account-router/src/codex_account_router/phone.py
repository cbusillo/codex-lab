"""Plain-text choices for native Shortcuts; selections travel through stdin."""


def choices(status):
    result = {}
    for task in status["tasks"]:
        if task["state"] not in ("idle", "notLoaded", "systemError"):
            continue
        name = " ".join("".join(c if c.isprintable() else " " for c in task["name"]).split())
        for account in status["accounts"]:
            title = f"{name[:80]} → {account} · {task['thread']}"
            result[title] = {"thread": task["thread"], "execution": account}
    return result
