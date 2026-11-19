import pytest
from spelling_guard import restore_diacritics


@pytest.mark.parametrize('raw,cleaned,expected',[
    ('pušni ten branch','Pusni ten branch.','Pušni ten branch.'),
    ('pušni ten branch','Pushni ten branch.','Pušni ten branch.'),
    ('um pušni ten branch','Pušni ten branch.','Pušni ten branch.'),
    ('um pušni ten branch','Pusni ten branch.','Pušni ten branch.'),
    ('Říkal čárka ano','Řikal, ano.','Říkal, ano.'),
    ('Pušni','Pušní!','Pušní!'),
    ('Pušní','Pušni!','Pušní!'),
    ('Holesovice','Holešovice.','Holešovice.'),
    ('Holoshowice','Holešovice.','Holešovice.'),
    ('Jiri','Jiří.','Jiří.'),
    ('Holesovíce','Holešovíce.','Holešovíce.'),
    ('Deployni','Deploy.','Deploy.'),  # No loanword dictionary in the guard.
    ('mám pět minut','Mám 5 minut.','Mám 5 minut.'),
])
def test_restores_only_stripped_or_changed_accents(raw,cleaned,expected):
    assert restore_diacritics(raw,cleaned)==expected


def test_ambiguous_source_rejects_instead_of_guessing():
    assert restore_diacritics('pušni pušní','Pusni.') is None


def test_cleanup_follows_drift_guard_and_user_edited_prompt():
    import voice_gateway as vg
    from test_voice_gateway import chat_server
    raw='um pušni ten branch'
    with chat_server('Pusni ten branch.') as (url,_):
        assert vg.clean(raw,[],url,'m',prompt='My edited prompt.')==('Pušni ten branch.','ok')
        assert vg.clean(raw,[],url,'m',prompt='My edited prompt.',drift_guard=False)==('Pusni ten branch.','ok')
    with chat_server('Meet Jiří in Holešovice.') as (url,_):
        assert vg.clean('Meet Jiri in Holesovice.',[],url,'m')==('Meet Jiří in Holešovice.','ok')


def test_sh_matching_is_only_from_raw_s_caron():
    assert restore_diacritics('check the čas balance','Check the cash balance.')=='Check the cash balance.'
    assert restore_diacritics('pušni','pusni')=='pušni'
    assert restore_diacritics('pušni','pushni')=='pušni'


def test_restore_keeps_mixed_cleaned_casing():
    assert restore_diacritics('um škodaauto has five jobs','SkodaAuto has 5 jobs.')=='ŠkodaAuto has 5 jobs.'
