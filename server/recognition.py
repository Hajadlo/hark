"""Speech evidence and language policy shared by the real recognizer and tests."""
from collections import Counter
from difflib import SequenceMatcher
import logging
import re
import unicodedata

import numpy as np
from whisper_confidence import confidence_available, mean_log_probability, prefer_english

SLAVIC = frozenset('sk pl sl hr sr bs ru uk be bg mk'.split())
NO_SPEECH_PHRASES = frozenset([
    'you', 'thank you', 'thanks for watching', 'thank you for watching',
    'titulky vytvořil johnyx', 'děkuji za pozornost', 'děkujeme za pozornost',
])


CZECH_SLANG_PROMPT = ('pushni, mergni, commitni, deployni, checkni, rebasni, pullni, buildni, '
                      'restartni, pingni, fetchni, squashni, revertni, debugni, testni, fixni, '
                      'releasni, reviewni, updatni, cherry-pickni.')
CZECH_NOUN_PROMPT = 'mainu branchi commitu requestu PRko stagingu buildu deploye repu issue ticketu logu runneru.'


def czech_initial_prompt(glossary, tokenize=None):
    # The list alone regressed public Czech; retain the baseline if the
    # glossary is absent rather than silently using that rejected variant.
    if not glossary:
        return None
    build = lambda terms: CZECH_SLANG_PROMPT+' '+CZECH_NOUN_PROMPT+' Názvy a termíny: '+', '.join(terms)+'.'
    terms = list(glossary)
    if tokenize:
        # Whisper otherwise silently retains only the last 224 prompt tokens.
        # Keep complete recent glossary entries and the measured dev seeds;
        # cleanup still sees the entire glossary. Reserve one special token.
        while terms and len(tokenize(' '+build(terms)))>223:
            terms.pop(0)
        if not terms:
            return None
    return build(terms)


def english_initial_prompt(glossary, tokenize=None):
    """Whisper prompt that spells the glossary for a second, hinted English pass."""
    if not glossary:
        return None
    build = lambda terms: 'Names and terms: '+', '.join(terms)+'.'
    terms = list(glossary)
    if tokenize:
        # Same 224-token limit as the Czech hint: drop the oldest terms first.
        while terms and len(tokenize(' '+build(terms)))>223:
            terms.pop(0)
        if not terms:
            return None
    return build(terms)


def sounds_alike(heard, spelled):
    """Rough English sound match: the letters, or the consonant skeleton (sculpt ~ skald)."""
    def skeleton(word):
        word = unicodedata.normalize('NFD', word)
        word = ''.join(c for c in word if not unicodedata.combining(c))
        for a, b in (('ph', 'f'), ('ck', 'k'), ('c', 'k'), ('q', 'k'), ('z', 's'), ('w', 'v')):
            word = word.replace(a, b)
        word = re.sub(r'[aeiouy\W_]', '', word)
        return re.sub(r'(.)\1+', r'\1', word)
    ratio = lambda x, y: SequenceMatcher(None, x, y, autojunk=False).ratio() if x and y else 0
    return max(ratio(heard, spelled), ratio(skeleton(heard), skeleton(spelled))) >= 0.5


def glossary_respellings(candidate, hinted, terms):
    """Take from the hinted pass only one-for-one respellings into glossary words.

    Intended behaviour: one heard word may become one glossary word that sounds
    like it (Morg -> Mork, sculpt -> skald), keeping the first transcript's
    punctuation. Every other difference keeps the first transcript's words, so
    the hint can never add, drop or change a word that is not a name. Merges
    (bad boom -> Kubernetes) are left to cleanup and its heard-as hints: a merge
    can swallow a word like "not"."""
    key = lambda w: re.sub(r'^[^\w]+|[^\w]+$', '', w.casefold())
    words = {key(w) for term in terms for w in term.split() if key(w)}
    core = lambda w: re.sub(r'^[^\w]+|[^\w]+$', '', w)
    framed = lambda heard, new: re.match(r'^[^\w]*', heard).group() + core(new) + re.search(r'[^\w]*$', heard).group()
    a, b = candidate.split(), hinted.split()
    out = []
    for tag, i1, i2, j1, j2 in SequenceMatcher(None, [key(w) for w in a], [key(w) for w in b], autojunk=False).get_opcodes():
        heard, new = a[i1:i2], b[j1:j2]
        if tag == 'replace' and len(heard) == len(new) <= 3 and all(key(w) in words for w in new) \
                and all(sounds_alike(key(h), key(n)) for h, n in zip(heard, new)):
            out.extend(framed(h, n) for h, n in zip(heard, new))
        else:
            out.extend(heard)
    return ' '.join(out)


def preserve_capitals(candidate, hinted):
    """Hints supply spelling, not a reason to lose the candidate's capitals."""
    words = lambda text: list(re.finditer(r'[^\W_]+', text))
    before, after = words(candidate), words(hinted)
    if not before or not after:
        return hinted
    key = lambda word: ''.join(c for c in unicodedata.normalize('NFD', word.group().casefold())
                               if not unicodedata.combining(c))
    matcher = SequenceMatcher(None, list(map(key, before)), list(map(key, after)), autojunk=False)
    pairs = {}
    for block in matcher.get_matching_blocks():
        pairs.update((block.b+i, block.a+i) for i in range(block.size))
    for tag, a, b, c, d in matcher.get_opcodes():
        if tag=='replace' and b-a==d-c:
            pairs.update(zip(range(c,d),range(a,b)))
        elif tag=='replace' and re.search(r'[.!?]\s*$',candidate[:before[a].start()]):
            pairs[c]=a
    pairs[0] = 0  # The opening word can change spelling completely.
    chars = list(hinted)
    for target, source in pairs.items():
        raw, new = before[source].group(), after[target].group()
        for i, letter in enumerate(new):
            upper = raw[i].isupper() if len(raw)==len(new) else raw.isupper() or (i==0 and raw[0].isupper())
            if upper:
                chars[after[target].start()+i] = letter.upper()
    return ''.join(chars)


def allowed_languages(value, capabilities):
    if not capabilities.supports_language_detect:
        return ()
    languages = tuple(dict.fromkeys(code.strip().lower() for code in value.split(',') if code.strip()))
    if not languages or any(code not in capabilities.languages for code in languages):
        raise ValueError('VOICE_GATEWAY_LANGUAGES must contain supported language codes.')
    return languages


def closest_language(detected, allowed):
    if detected in allowed:
        return detected
    preferred = 'cs' if detected in SLAVIC else 'en'
    return preferred if preferred in allowed else allowed[0]


def frame_energy(pcm):
    samples = np.asarray(pcm, dtype=np.float32)
    count = len(samples) // 320
    frames = samples[:count*320].reshape(count, 320)
    energy = np.sqrt(np.einsum('ij,ij->i', frames, frames) / 320)
    if len(samples) % 320:
        energy = np.append(energy, np.sqrt(np.mean(samples[count*320:]**2)))
    return energy


def speech_evidence(pcm):
    """Admit short/trimmed speech; pitch is evidence, never an admission rule.

    A fixed low floor cannot rise onto trimmed push-to-talk speech.
    Bounded 40 ms probes leave enough samples for low-pitch autocorrelation
    and keep this independent of recording length after the vectorized RMS.
    """
    energy = frame_energy(pcm)
    if not len(energy):
        return False, False
    active = np.flatnonzero(energy >= 0.00005)
    if len(active) < 2:  # A single impulse is not a word; 40 ms can be one.
        return False, False
    samples = np.asarray(pcm, dtype=np.float32)
    if len(samples) < 640:
        return True, False
    # Include both loudest and spread-out frames so a brief word in a long
    # recording can provide evidence without inspecting every frame in Python.
    indices = np.unique(np.concatenate((active[np.linspace(0,len(active)-1,min(32,len(active)),dtype=int)],
                                        active[np.argsort(energy[active])[-32:]])))
    starts = np.minimum(indices*320, len(samples)-640)
    frames = samples[starts[:,None]+np.arange(0,640,4)]
    audible = np.all(np.mean(frames.reshape(-1,2,80)**2,axis=2)>=1e-12,axis=1)
    frames = frames-frames.mean(axis=1,keepdims=True)
    spectrum = abs(np.fft.rfft(frames*np.hanning(160)))**2
    peak = spectrum.argmax(axis=1)
    tone = np.take_along_axis(spectrum, np.clip(peak[:,None]+np.arange(-3,4),0,80),axis=1).sum(axis=1)
    # A tone ending halfway through a probe spreads its FFT like speech;
    # its silent half must not turn that edge into clear voiced evidence.
    rich = audible & (tone < .99*spectrum.sum(axis=1))
    for lag in range(10,51):  # 80–400 Hz at 4 kHz; one voiced 40 ms probe suffices.
        left, right = frames[:,:-lag], frames[:,lag:]
        power = np.sqrt(np.einsum('ij,ij->i',left,left)*np.einsum('ij,ij->i',right,right))
        correlation = np.einsum('ij,ij->i',left,right)/np.maximum(power,1e-20)
        if np.any(rich & (correlation >= .45)):
            return True, True
    return True, False


def has_speech(pcm):
    return speech_evidence(pcm)[0]


def no_speech_text(text):
    words = ' '.join(re.findall(r'[^\W_]+',text.casefold()))
    return not words or words in NO_SPEECH_PHRASES


def speech_windows(pcm):
    """Cover every sample, cut at the quietest late point, leave ≥1 s tail."""
    start = 0
    while start < len(pcm):
        end = min(start+30*16000,len(pcm))
        if end < len(pcm):
            end = min(end,len(pcm)-16000)
            first = max(start+16000,end-5*16000)
            energy = frame_energy(pcm[first:end])
            if len(energy):
                # Use an 80 ms neighborhood so a consonant's brief dip does
                # not win over a real pause. Latest minimum breaks ties.
                quiet = np.convolve(energy,np.ones(4)/4,mode='valid')
                quietest = len(quiet)-1-int(np.argmin(quiet[::-1]))
                end = first+quietest*320+640
        yield pcm[start:end]
        start = end


class Recognizer:
    def __init__(self, model, languages, whisper_options=None):
        self.model = model
        self.session = model.session()
        self.languages = allowed_languages(languages, model.capabilities)
        self.whisper_options = whisper_options if model.arch == 'whisper' else None
        self.confidence_active = True
        self._check_confidence = False

    def warmup(self, pcm, czech_prompt=None, english_prompt=None):
        self.run(pcm, warmup=True)
        if self.whisper_options and english_prompt and 'en' in self.languages:
            try:
                self.session.run(pcm, language='en', timestamps='none',
                                 family=self.whisper_options(initial_prompt=english_prompt, condition_on_prev_tokens=False))
            except Exception as error:
                logging.warning('Optional English warm-up failed (%s).',type(error).__name__)
        if self.whisper_options and 'cs' in self.languages:
            # Detection does not compile the forced/hinted decoder paths.
            options = [{}]
            if czech_prompt:
                options.append({'family':self.whisper_options(initial_prompt=czech_prompt, condition_on_prev_tokens=False)})
            for option in options:
                try:
                    self.session.run(pcm, language='cs', timestamps='none', **option)
                except Exception as error:
                    logging.warning('Optional Czech warm-up failed (%s).',type(error).__name__)

    def run(self, pcm, *, warmup=False, czech_prompt=None, english_prompt=None, english_terms=()):
        self._check_confidence = warmup and self.model.arch=='whisper'
        admitted, clear_voice = (True,False) if warmup else speech_evidence(pcm)
        if not admitted:
            return '', None
        options = {'timestamps':'none'}
        long_whisper = self.whisper_options and len(pcm)>30*16000
        if long_whisper:
            options.update(timestamps='segment',family=self.whisper_options(condition_on_prev_tokens=False))
        texts, languages = [], Counter()
        for window in speech_windows(pcm) if long_whisper else (pcm,):
            if long_whisper:
                # Never apply short-word/pitch admission to a fragment. Only
                # clearly silent windows may be skipped after global admission.
                if not np.any(frame_energy(window)>=0.000001):
                    continue
                _, clear_voice = speech_evidence(window)
            text, language = self.decode(window,options)
            if self.whisper_options and not warmup and not clear_voice and no_speech_text(text):
                text = ''
            if text and language=='cs' and czech_prompt and self.whisper_options and not warmup:
                # Freeze the unprompted choice first. Prompt-conditioned
                # probabilities must never influence English/Czech selection.
                try:
                    hinted_options={**options,'family':self.whisper_options(initial_prompt=czech_prompt,condition_on_prev_tokens=False)}
                    hinted=self.session.run(window,language='cs',**hinted_options).text.strip()
                    if hinted and (clear_voice or not no_speech_text(hinted)):text=preserve_capitals(text,hinted)
                except Exception as error:
                    logging.warning('Optional Czech prompt failed; keeping candidate (%s).',type(error).__name__)
            if text and language=='en' and english_prompt and self.whisper_options and not warmup:
                # As for Czech: the language is already chosen without the hint.
                try:
                    hinted_options={**options,'family':self.whisper_options(initial_prompt=english_prompt,condition_on_prev_tokens=False)}
                    hinted=self.session.run(window,language='en',**hinted_options).text.strip()
                    if hinted and (clear_voice or not no_speech_text(hinted)):
                        text=preserve_capitals(text,glossary_respellings(text,hinted,english_terms))
                except Exception as error:
                    logging.warning('Optional English prompt failed; keeping candidate (%s).',type(error).__name__)
            if text:
                texts.append(text)
                if language:languages[language] += len(window)
        return ' '.join(texts), languages.most_common(1)[0][0] if languages else None

    def decode(self, pcm, options):
        forced = self.languages[0] if len(self.languages)==1 else None
        result = self.session.run(pcm,language=forced,**options)
        if self._check_confidence:
            self.confidence_active = confidence_available(self.session,result)
            logging.info('Whisper confidence selection %s.', 'active' if self.confidence_active else 'inactive')
            self._check_confidence = False
        if not self.languages:
            return result.text.strip(), None
        language = forced or closest_language(result.language,self.languages)
        if not forced and result.language != language:
            result = self.session.run(pcm,language=language,**options)
        if self.confidence_active and not forced and self.model.arch=='whisper' and language!='en' and 'en' in self.languages:
            confidence = mean_log_probability(self.session,result)
            if confidence is not None:
                try:
                    probe = dict(options)
                    if self.whisper_options:
                        # Wrong-language probes often exhaust all temperature
                        # tiers. One greedy pass is enough to compare confidence.
                        probe['family'] = self.whisper_options(condition_on_prev_tokens=False,temperature=0,temperature_inc=0)
                    english = self.session.run(pcm,language='en',**probe)
                    if english.text.strip() and prefer_english(confidence,mean_log_probability(self.session,english)):
                        # Winning probes are decoded normally, so the cheap
                        # comparison does not weaken transcription safeguards.
                        result = self.session.run(pcm,language='en',**options) if self.whisper_options else english
                        language = 'en'
                except Exception as error:
                    # Comparison is optional: a good candidate must survive
                    # either a probe failure or a failed normal winner rerun.
                    logging.warning("Optional English decode failed; keeping candidate (%s).",type(error).__name__)
        return result.text.strip(),language
