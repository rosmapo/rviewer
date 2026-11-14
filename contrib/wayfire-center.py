#!/usr/bin/env python3
"""Nastaví veľkosť a vycentruje okno rviewera na jeho výstupe (v pracovnej ploche bez panelov) cez Wayfire IPC.

Veľkosť aj poloha sa menia JEDNÝM volaním (configure-view), takže okno pri zmene obrázka neskáče.
Cieľová veľkosť sa berie z RVIEWER_WIDTH / RVIEWER_HEIGHT (logické px); bez nich sa použije aktuálna.

Použitie v rviewer.conf:
    window-place-command=python3 ~/.config/rviewer/wayfire-center.py

Komunikácia je rovnaká ako vo wfctl. Ladenie: RVIEWER_DEBUG=1 vypíše odpovede Wayfiru.
Okno sa hľadá podľa PID (RVIEWER_PID, ak ho Wayfire v zozname uvádza), inak podľa app-id
(predvolene "rviewer", zmena: RVIEWER_APP_ID).
"""
import json
import os
import socket
import struct
import sys

APP_ID = os.environ.get("RVIEWER_APP_ID", "rviewer")
DEBUG = bool(os.environ.get("RVIEWER_DEBUG"))


def request(method, data=None):
    path = os.environ.get("WAYFIRE_SOCKET")
    if not path:
        sys.exit("WAYFIRE_SOCKET nie je nastavený")
    msg = json.dumps({"method": method, "data": data or {}}).encode()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.connect(path)
        s.sendall(struct.pack("<I", len(msg)) + msg)
        header = s.recv(4, socket.MSG_WAITALL)
        if len(header) != 4:
            sys.exit("Wayfire nevrátil platnú odpoveď")
        (n,) = struct.unpack("<I", header)
        buf = b""
        while len(buf) < n:
            chunk = s.recv(n - len(buf))
            if not chunk:
                sys.exit("Wayfire prerušil spojenie")
            buf += chunk
    res = json.loads(buf)
    if DEBUG:
        print(f"{method} {data} -> {res}", file=sys.stderr)
    return res


def rect(d):
    return d["x"], d["y"], d["width"], d["height"]


def main():
    views = request("window-rules/list-views")
    pid = os.environ.get("RVIEWER_PID")
    mine = [v for v in views if pid and str(v.get("pid")) == pid] or [v for v in views if v.get("app-id") == APP_ID]
    if not mine:
        sys.exit(f"nenašiel som okno s app-id '{APP_ID}'")
    view = mine[0]
    vx, vy, vw, vh = rect(view["geometry"])
    # Cieľová veľkosť z rviewera (okno sa zmení v tom istom kroku, v ktorom sa presunie).
    tw = int(os.environ.get("RVIEWER_WIDTH") or vw)
    th = int(os.environ.get("RVIEWER_HEIGHT") or vh)

    # Oblasť výstupu: preferuj workarea (bez panelov), inak celú geometriu.
    outputs = request("window-rules/list-outputs")
    out = next((o for o in outputs if o.get("id") == view.get("output-id")), outputs[0])
    ox, oy, ow, oh = rect(out.get("workarea") or out["geometry"])

    x = ox + (ow - tw) // 2
    y = oy + (oh - th) // 2
    if (x, y, tw, th) == (vx, vy, vw, vh):
        return

    # Najprv vnorené "geometry"; ak Wayfire odmietne, ploché x/y/width/height (ako v nápovede wfctl).
    res = request("window-rules/configure-view",
                  {"id": view["id"], "geometry": {"x": x, "y": y, "width": tw, "height": th}})
    if "error" in res:
        res = request("window-rules/configure-view",
                      {"id": view["id"], "x": x, "y": y, "width": tw, "height": th})
    if "error" in res:
        print(f"wayfire-center: {res['error']}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
