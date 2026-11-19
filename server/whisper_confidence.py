"""Optional confidence adapter; binding changes must not break dictation."""
import ctypes
import logging
import math

ENGLISH_BIAS = 0.05
_warned = False


def _unavailable(error):
    global _warned
    if not _warned:
        logging.warning('Whisper confidence unavailable; keeping detected language (%s).',type(error).__name__)
        _warned = True


def _native_traces(session):
    import transcribe_cpp as tc
    traces=[]
    for index in range(tc._lib.transcribe_get_whisper_chunk_count(session._h)):
        trace=tc._generated.transcribe_whisper_chunk_trace()
        tc._lib.transcribe_whisper_chunk_trace_init(ctypes.byref(trace))
        status=tc._lib.transcribe_get_whisper_chunk_trace(session._h,index,ctypes.byref(trace))
        if status:raise RuntimeError('Whisper trace status '+str(status))
        traces.append(trace)
    return traces


def _token_score(result):
    tokens=[t for t in getattr(result,'tokens',()) if t.id<50257]
    if tokens and any(t.p>0 for t in tokens):
        return sum(math.log(max(1e-12,min(1,t.p))) for t in tokens)/len(tokens)
    return None


def mean_log_probability(session, result):
    try:
        score=_token_score(result)
        if score is not None:return score
        # 0.3.1 Whisper leaves token rows empty. Read the generated C trace
        # before another run overwrites it; its mean includes generated EOS.
        traces=_native_traces(session)
        weighted=[]
        for index,trace in enumerate(traces):
            # Timestamp continuation can reopen an overlapping encoder
            # window. Only audio up to the next start belongs to this trace.
            end=min(trace.t1_ms,traces[index+1].t0_ms) if index+1<len(traces) else trace.t1_ms
            duration=max(0,end-trace.t0_ms)
            if duration and not trace.no_speech_triggered:
                weighted.append((trace.avg_logprob,duration))
        return sum(p*w for p,w in weighted)/sum(w for _,w in weighted) if weighted else None
    except Exception as error:
        _unavailable(error)
        return None


def confidence_available(session, result):
    try:
        if _token_score(result) is None:_native_traces(session)
        return True  # An empty/no-speech warm-up can still have a valid API.
    except Exception as error:
        _unavailable(error)
        return False


def prefer_english(candidate, english, bias=ENGLISH_BIAS):
    return candidate is not None and english is not None and english+bias>=candidate
