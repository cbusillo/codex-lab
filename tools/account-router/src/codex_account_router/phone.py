"""Plain-text choices for native Shortcuts; selections travel through stdin."""


def account_name(status, label):
    email = status.get("accountNames", {}).get(label)
    if not isinstance(email, str):
        return label
    email = " ".join("".join(c if c.isprintable() else " " for c in email).split())[:160]
    return f"{email} ({label})" if email else label


def choices(status):
    result = {}
    for task in status["tasks"]:
        if task["state"] not in ("idle", "notLoaded", "systemError"):
            continue
        name = " ".join("".join(c if c.isprintable() else " " for c in task["name"]).split())
        for account in status["accounts"]:
            title = f"{name[:80]} → {account_name(status, account)} · {task['thread']}"
            result[title] = {"thread": task["thread"], "execution": account}
    return result
