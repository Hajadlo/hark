"""Restore lost word accents without undoing glossary, punctuation or filler edits."""
from collections import Counter, defaultdict
from difflib import SequenceMatcher
import re
import unicodedata


def _fold(word):
    return ''.join(c for c in unicodedata.normalize('NFD',word.casefold()) if not unicodedata.combining(c))


def _accents(word):
    return {i:c for i,c in enumerate(word.casefold()) if any(unicodedata.combining(d) for d in unicodedata.normalize('NFD',c))}


def _casing(source,target):
    if len(source)==len(target):
        return ''.join(c.upper() if t.isupper() else c.lower() for c,t in zip(source,target))
    if target.isupper():return source.upper()
    if target.istitle():return source.capitalize()
    if target.islower():return source.lower()
    return source


def restore_diacritics(raw, cleaned):
    """Return repaired cleanup, or None if the matching source word is ambiguous."""
    before=list(re.finditer(r'[^\W_]+',unicodedata.normalize('NFC',raw)))
    cleaned=unicodedata.normalize('NFC',cleaned)
    after=list(re.finditer(r'[^\W_]+',cleaned))
    old_keys=[_fold(w.group()) for w in before]
    # Only a raw š may acquire an sh alias. Plain English cash must never
    # collapse to cas and accidentally match Czech čas.
    aliases=[_fold(w.group().casefold().replace('š','sh')) for w in before]
    new_keys=[_fold(w.group()) for w in after]
    variants=defaultdict(set)
    old_counts=Counter()
    for w,key,alias in zip(before,old_keys,aliases):
        for k in {key,alias}:
            variants[k].add(w.group().casefold())
            old_counts[k]+=1
    new_counts=Counter(new_keys)
    repairs=[]
    used_before,used_after=set(),set()
    # Prefer ordinary letter matches; use directional transliterations only
    # for words left unmatched, so an existing English word keeps its match.
    for keys in (old_keys,aliases):
        for block in SequenceMatcher(None,keys,new_keys,autojunk=False).get_matching_blocks():
            for offset in range(block.size):
                i,j=block.a+offset,block.b+offset
                if i in used_before or j in used_after:continue
                used_before.add(i);used_after.add(j)
                source,target=before[i].group(),after[j]
                marks=_accents(source)
                if not marks or all(_accents(target.group()).get(k)==c for k,c in marks.items()):
                    continue  # Missing accents may be added, including via glossary.
                key=keys[i]
                if len(variants[key])>1 and old_counts[key]!=new_counts[key]:return None
                repairs.append((target.start(),target.end(),_casing(source,target.group())))
    for start,end,word in sorted(repairs,reverse=True):cleaned=cleaned[:start]+word+cleaned[end:]
    return cleaned
