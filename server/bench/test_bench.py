"""Check metric behavior where language bake-off conclusions depend on it."""
import importlib.util
from pathlib import Path

spec = importlib.util.spec_from_file_location('bench', Path(__file__).with_name('bench.py'))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)

def test_diacritics_are_errors_and_unicode_composition_is_not():
    assert bench.score('Jiří', 'Jiri')['word_errors'] == 1
    assert bench.score('Jiří', 'Jiri')['char_errors'] == 2
    assert bench.score('Jiří', 'Jir\u030ci\u0301')['word_errors'] == 0

def test_case_punctuation_and_whitespace_do_not_change_scores():
    assert bench.score("Let's pushni ten branch!", "let's  pushni ten branch.")['word_errors'] == 0

def test_multiword_names_require_whole_tokens_and_keep_diacritics():
    assert bench.score('', 'České Budějovice', ['České Budějovice'])['name_hits'] == 1
    assert bench.score('', 'České Budějovicex', ['České Budějovice'])['name_hits'] == 0
    assert bench.score('', 'Ceske Budejovice', ['České Budějovice'])['name_hits'] == 0

def test_empty_and_insertion_deletion_counts():
    assert bench.distance([], ['a', 'b']) == 2
    assert bench.distance(['a', 'b'], []) == 2
    assert bench.distance(['a', 'b', 'c'], ['a', 'd']) == 2
