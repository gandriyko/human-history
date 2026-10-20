"""Keyboard-layout independence: shortcuts typed on a Cyrillic (ЙЦУКЕН) layout act like the Latin
key in the same physical position, so `q`, `/`, `n`, `Ctrl+U`… work without switching layouts."""

from __future__ import annotations

from textual import events
from textual.keys import _character_to_key

# Ukrainian ЙЦУКЕН rows against QWERTY, plus the Russian-only letters (ы ъ э ё).
_CYRILLIC = "йцукенгшщзхїфівапролджєячсмитьбю" + "ыъэё"
_LATIN = "qwertyuiop[]asdfghjkl;'zxcvbnm,." + "s]'`"

CHAR_MAP: dict[str, str] = dict(zip(_CYRILLIC, _LATIN, strict=True))
CHAR_MAP |= {c.upper(): latin.upper() for c, latin in zip(_CYRILLIC, _LATIN, strict=True) if latin.isalpha()}
# On ЙЦУКЕН the "/" key types "." and Shift+/ types ",".
CHAR_MAP |= {".": "/", ",": "?"}


def latin_key(event: events.Key, typing: bool) -> events.Key:
    """The Latin-layout equivalent of ``event``.

    While ``typing`` in a text input, plain characters are left alone (so Cyrillic text can be
    typed), but modifier combos such as ``ctrl+г`` are still mapped (to ``ctrl+u``).
    """
    modifiers, plus, base = event.key.rpartition("+")
    if plus:
        latin = CHAR_MAP.get(base)
        if latin is None:
            return event
        return events.Key(f"{modifiers}+{_character_to_key(latin.lower())}", event.character)
    if typing or event.character is None:
        return event
    latin = CHAR_MAP.get(event.character)
    if latin is None:
        return event
    return events.Key(_character_to_key(latin), latin)
