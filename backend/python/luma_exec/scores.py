"""luma.scores: your other scores, to read.

`find` lists them and `open` reads one. A reference is read-only: it shows
what a show did and on which lights, so it can be learned from or adapted to
the rig that is open. Nothing here changes a score.
"""

from __future__ import annotations

import json
import re
from collections import OrderedDict

from .score import BarGrid, Clip, _plain

_OUTPUTS = ("color", "aim", "strobe", "laser")
# A clip placed once per hit is named for its look plus where it sits:
# "Chant @4.4.1.84", "Green pickup 34.4.3", "Break floor 8".
_PLACE = re.compile(r"[\s@·–-]*\d[\d.\s@·–-]*$")


def _look(name):
    return _PLACE.sub("", name).strip() or name


def _groups(selection):
    expression = str(dict(selection).get("expression", "all"))
    return tuple(part.strip() for part in expression.split("|") if part.strip())


def _output(graph):
    for node in (graph.get("nodes") or {}).values():
        if node.get("kind") in _OUTPUTS:
            return node["kind"]
    return "?"


class Found(tuple):
    """What `find` returned. Index it, and pass an item to `open`."""

    def __repr__(self):
        if not self:
            return "no other scores of yours match"
        lines = [f"{len(self)} score{'' if len(self) == 1 else 's'}"]
        lines += [f"  [{index}] {item}" for index, item in enumerate(self)]
        lines.append("open one with luma.scores.open(found[0])")
        return "\n".join(lines)


class FoundScore:
    """One score in a `find` result."""

    __slots__ = ("id", "name", "track_id", "track", "artist", "venue", "clips", "edited")

    def __init__(self, row):
        self.id = row["id"]
        self.name = row.get("name") or ""
        self.track_id = row["track_id"]
        self.track = row.get("track_title") or ""
        self.artist = row.get("track_artist") or ""
        self.venue = row.get("venue_name") or ""
        self.clips = int(row.get("clips") or 0)
        self.edited = str(row.get("updated_at") or "")[:10]

    def __repr__(self):
        title = f"{self.track} — {self.artist}" if self.artist else self.track
        name = f' · "{self.name}"' if self.name else ""
        return f"{title} @ {self.venue}{name} · {self.clips} clips · edited {self.edited}"


class ReferenceVenue:
    """The rig a reference score plays on: its name and what each group is."""

    __slots__ = ("id", "name", "_groups")

    def __init__(self, values):
        self.id = values["id"]
        self.name = values.get("name") or ""
        self._groups = tuple(values.get("groups") or ())

    def groups(self):
        """Each group: name, fixtures, cells (0 for whole fixtures) and models."""
        return tuple(dict(group) for group in self._groups)

    def describe(self):
        lines = [f"{self.name}: {len(self._groups)} groups"]
        for group in self._groups:
            part = f"{group['cells']} cells of " if group["cells"] else ""
            models = ", ".join(group["models"]) or "nothing"
            lines.append(f"  {group['name']}: {part}{group['fixtures']} fixtures ({models})")
        return "\n".join(lines)

    def __repr__(self):
        return f"<venue {self.name!r} of a reference score; describe(), groups()>"


class Reference:
    """One of your other scores, read-only.

    `looks()` is the place to start: each distinct look, how often it plays,
    on which groups and where first. `roles()` turns that round: what each
    group of that rig is made of and which looks it carries. `clips` are
    ordinary Clip values, so `clip.graph` goes straight into
    `edit.add_clip(clip.graph, name=clip.name, at=clip.at, selection=...)`
    on the open track; `clip.graph.source()` prints its Python. Positions are
    this track's when the reference is the same song (`same_track`);
    otherwise they count four beats to a bar from the song's first downbeat.
    """

    def __init__(self, values, bars):
        self.id = values["id"]
        self.name = values.get("name") or ""
        self.track = values.get("track_title") or ""
        self.same_track = bool(values.get("same_track"))
        self.venue = ReferenceVenue(values["venue"])
        self._document = _plain(values["document"])
        self._bars = bars

    @property
    def clips(self):
        """Every clip, ordered by start, layer z, then id."""
        return tuple(Clip.read(id, clip, self._bars) for id, clip in sorted(
            self._document["clips"].items(),
            key=lambda item: (item[1]["start"], item[1].get("z_index", 0), item[0])))

    def source(self):
        """The score JSON, with groups named."""
        return json.dumps(self._document, indent=2, sort_keys=True)

    def _by_look(self):
        looks = OrderedDict()
        for id, clip in sorted(self._document["clips"].items(),
                               key=lambda item: (item[1]["start"], item[0])):
            entry = looks.setdefault(_look(clip.get("name", "")), {
                "count": 0, "groups": OrderedDict(), "output": _output(clip.get("graph") or {}),
                "beats": [], "first": clip["start"], "last": clip["start"], "blend": set(), "id": id})
            entry["count"] += 1
            entry["beats"].append(clip["duration"])
            entry["last"] = clip["start"]
            entry["blend"].add(clip.get("blend_mode", "replace"))
            for group in _groups(clip.get("selection", {"expression": "all"})):
                entry["groups"][group] = entry["groups"].get(group, 0) + 1
        return looks

    def looks(self):
        """Each distinct look: clips, output, typical length, groups, span.

        Clips named for one look and a place ("Snare 17.2", "Snare 17.4") are
        one look. `look(name)` gives one of its clips to read.
        """
        lines = [f"{self!r}"]
        for name, entry in self._by_look().items():
            beats = sorted(entry["beats"])[len(entry["beats"]) // 2]
            span = self._bars.format(entry["first"])
            if entry["count"] > 1:
                span += f" to {self._bars.format(entry['last'])}"
            lines.append(
                f"  {name}: {entry['count']}x {entry['output']} {'/'.join(sorted(entry['blend']))}, "
                f"{beats:g} beats, on {' | '.join(entry['groups'])}, {span}")
        return "\n".join(lines)

    def look(self, name):
        """The first clip of a look, by the name `looks()` prints."""
        entry = self._by_look().get(name)
        if entry is None:
            raise KeyError(f"no look named {name!r}; looks() lists them")
        return Clip.read(entry["id"], self._document["clips"][entry["id"]], self._bars)

    def roles(self):
        """What each group of that rig is made of and which looks it carries."""
        carried = {}
        for name, entry in self._by_look().items():
            for group, count in entry["groups"].items():
                carried.setdefault(group, []).append(f"{name} {count}x")
        made = {group["name"]: group for group in self.venue.groups()}
        lines = [f"{self.venue.name}: what each group did"]
        for group in list(made) + [g for g in carried if g not in made]:
            info = made.get(group)
            if info is None:
                what = "the whole rig" if group == "all" else "a group that no longer exists"
            else:
                part = f"{info['cells']} cells of " if info["cells"] else ""
                what = f"{part}{info['fixtures']} fixtures ({', '.join(info['models']) or 'nothing'})"
            looks = ", ".join(carried.get(group, [])) or "unused"
            lines.append(f"  {group}: {what}: {looks}")
        return "\n".join(lines)

    def __repr__(self):
        name = f"{self.name!r} · " if self.name else ""
        return (f"<reference {name}{self.track!r} @ {self.venue.name} "
                f"clips={len(self._document['clips'])} read-only; looks(), roles(), clips, venue>")


class Scores:
    """Your other scores. `find()` lists them; `open(found[0])` reads one."""

    def __init__(self, track):
        self._track = track

    def find(self, text=None, *, track=None):
        """Your scores, this song's first and then the newest.

        `text` matches the score, song, artist or venue name; `track` is a
        track id, such as `luma.track.id` for this song on other rigs. The
        score you are editing is left out.
        """
        answer = self._track._call("track.scores_find", {
            "text": None if text is None else str(text),
            "track_id": None if track is None else str(getattr(track, "id", track)),
        })
        return Found(FoundScore(row) for row in answer["scores"])

    def open(self, score):
        """Read one score: a `find` item, or a score id."""
        answer = self._track._call("track.scores_open", {"id": str(getattr(score, "id", score))})
        bars = self._track.bars if answer.get("same_track") else BarGrid((0.0,), 4)
        return Reference(answer, bars)

    def __repr__(self):
        return "<luma.scores: find(text=None, track=None), open(score)>"
