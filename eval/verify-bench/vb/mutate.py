"""Deterministic, bilingual (VI/EN) error injection with exact labels (SymAI-style).

Every mutator takes one clean bullet (+ meeting context) and returns the mutated text, or None
when the bullet has no eligible target. The edit is a single, local, rule-defined change, so the
label (`type`) is exact by construction: the mutated bullet is no longer supported by the
transcript in exactly the way the type names. Fabrications are drawn from fixed template pools
and checked to be absent from the transcript.

Types (SymAI rule in brackets):
  number        number / unit change                         [ACC-NUM-01]
  entity        name / product / organisation swap           [ACC-ENT-01]
  negation      polarity flip (VI không/chưa/chẳng, EN not)  [ACC-CON-01]
  hedge_drop    certainty inflation (VI có lẽ/chắc/hình như) [ACC-HDG-01]
  hedge_add     certainty deflation (adds a hedge)
  quantifier    scope change (VI tất cả/một số/nhiều)        [ACC-QNT-01]
  temporal      date / weekday / tense / "tuần sau" shift     [ACC-TMP-01]
  speaker       speaker misattribution                       [ACC-SPK-01]
  fabricate     unsupported claim inserted                   [ACC-FAB-01]
  overgeneralise episodic -> habitual, one person -> everyone [ACC-GEN-01]
  cluster_insert two fabricated claims inserted together     [INT-INS-01]
"""

from __future__ import annotations

import random
import re
import unicodedata

VI_CHARS = set("ăâđêôơưàáạảãằắặẳẵầấậẩẫèéẹẻẽềếệểễìíịỉĩòóọỏõồốộổỗờớợởỡùúụủũừứựửữỳýỵỷỹ")


def lang_of(text: str) -> str:
    low = text.lower()
    n = sum(ch in VI_CHARS for ch in low)
    return "vi" if n >= 2 else "en"


def fold(s: str) -> str:
    s = unicodedata.normalize("NFD", s.lower()).replace("đ", "d")
    return "".join(c for c in s if unicodedata.category(c) != "Mn")


def _sub1(pat: str, repl, text: str, flags=re.IGNORECASE) -> str:
    return re.sub(pat, repl, text, count=1, flags=flags)


def _keepcase(src: str, repl: str) -> str:
    return repl[0].upper() + repl[1:] if src[:1].isupper() and repl else repl


def _apply_first(rules: list[tuple[str, str]], text: str) -> str | None:
    for pat, repl in rules:
        m = re.search(pat, text, flags=re.IGNORECASE)
        if m:
            new = text[: m.start()] + _keepcase(m.group(0), m.expand(repl)) + text[m.end():]
            new = re.sub(r"\s{2,}", " ", new).strip()
            return new if new != text else None
    return None


W = r"(?<![\wÀ-ỹ])"  # word start that understands Vietnamese letters
E = r"(?![\wÀ-ỹ])"

# --------------------------------------------------------------------------- number / unit

_EN_NUMW = {"two": "four", "three": "five", "four": "six", "five": "eight", "six": "nine", "seven": "ten",
            "eight": "twelve", "nine": "four", "ten": "twenty", "twelve": "twenty", "fifteen": "thirty",
            "twenty": "forty", "thirty": "sixty", "hundred": "thousand"}
_VI_NUMW = {"hai": "ba", "ba": "năm", "bốn": "sáu", "sáu": "chín", "bảy": "mười", "tám": "mười hai",
            "chín": "bốn", "mười": "hai mươi", "trăm": "nghìn", "nghìn": "triệu", "chục": "trăm"}
_UNIT_SWAP = {"en": [(r"\bmillion\b", "billion"), (r"\bbillion\b", "million"), (r"\bminutes?\b", "hours"),
                     (r"\bhours?\b", "days"), (r"\bdays?\b", "weeks"), (r"\bweeks?\b", "months"),
                     (r"\bthousand\b", "million"), (r"\bpercent\b", "times")],
              "vi": [(W + r"triệu" + E, "tỷ"), (W + r"tỷ" + E, "triệu"), (W + r"phút" + E, "giờ"),
                     (W + r"tiếng" + E, "ngày"), (W + r"giờ" + E, "ngày"), (W + r"tuần" + E, "tháng"),
                     (W + r"tháng" + E, "năm"), (W + r"nghìn" + E, "triệu"), (r"(?<=\d)tr\b", "tỷ"),
                     (r"(?<=\d)k\b", "tr")]}


def _bump(value: str) -> str:
    v = float(value.replace(",", "."))
    if v >= 1900 and v <= 2100 and v == int(v):  # a year: shift it
        return str(int(v) + 1)
    new = v * 2 if v else 3
    if v == int(v) and 1 <= v <= 3:
        new = v + 2
    s = str(int(new)) if new == int(new) else f"{new:g}"
    return s.replace(".", ",") if "," in value else s


def mut_number(b, ctx, rng):
    t, lang = b["text"], b["lang"]
    m = list(re.finditer(r"(?<![\d.,:/])(\d+(?:[.,]\d+)?)(?![\d:/])", t))
    if m:
        x = rng.choice(m)
        return t[: x.start(1)] + _bump(x.group(1)) + t[x.end(1):]
    for pat, repl in _UNIT_SWAP[lang]:
        if re.search(pat, t, flags=re.IGNORECASE):
            return _sub1(pat, repl, t)
    words = _VI_NUMW if lang == "vi" else _EN_NUMW
    for w, r in words.items():
        if re.search(W + re.escape(w) + E, t, flags=re.IGNORECASE):
            if lang == "vi" and w == "ba" and re.search(r"\bba\s+(mẹ|má|ba)\b", t, re.IGNORECASE):
                continue  # "ba" = father
            return _sub1(W + re.escape(w) + E, r, t)
    return None


# --------------------------------------------------------------------------- entity swap

_CAP = re.compile(r"(?<![\wÀ-ỹ])([A-ZĐ][\wÀ-ỹ]+(?:[ .-][A-ZĐ0-9][\wÀ-ỹ]*)*)")
_NOT_ENT = {"the", "a", "an", "s1", "s2", "s3", "s4", "s5", "s6", "i", "we", "they", "he", "she", "it", "this",
            "that", "there", "người", "chúng", "họ", "anh", "chị", "em", "cô", "chú", "ông", "bà", "nhóm",
            "team", "everyone", "mọi", "ai", "có", "không", "speaker", "mr", "ms", "mrs", "dr", "cr", "councillor",
            "mayor", "council", "staff", "chair", "officer", "on", "in", "at", "by", "for", "and", "but", "if"}
_WEEKDAYS = ("monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday", "january", "february",
             "march", "april", "may", "june", "july", "august", "september", "october", "november", "december")


def entities(text: str, first_ok: bool = False) -> list[str]:
    out = []
    for m in _CAP.finditer(text):
        e = m.group(1).strip(" .-")
        if not first_ok and m.start() == 0:
            continue
        low = e.lower()
        if low in _NOT_ENT or low.split()[0] in _NOT_ENT or low in _WEEKDAYS or low.startswith("thứ") \
                or re.fullmatch(r"s\d+", low) or len(e) < 2:
            continue
        out.append(e)
    return out


def mut_entity(b, ctx, rng):
    ents = entities(b["text"])
    if not ents:
        return None
    pool = [e for e in ctx["entities"] if fold(e) not in fold(b["evidence"]) and e not in ents]
    pool = [e for e in pool if (" " in e) == False or True]
    if not pool:
        return None
    target = rng.choice(ents)
    # prefer a replacement of the same "shape" (single token vs multi-token)
    same = [e for e in pool if (" " in e) == (" " in target)] or pool
    repl = rng.choice(sorted(set(same)))
    return b["text"].replace(target, repl, 1)


# --------------------------------------------------------------------------- negation

_EN_NEG = [
    (r"\bdid not\b", "did"), (r"\bdidn't\b", "did"), (r"\bdoes not\b", "does"), (r"\bdoesn't\b", "does"),
    (r"\bdo not\b", "do"), (r"\bdon't\b", "do"), (r"\bis not\b", "is"), (r"\bisn't\b", "is"),
    (r"\bwas not\b", "was"), (r"\bwasn't\b", "was"), (r"\bare not\b", "are"), (r"\baren't\b", "are"),
    (r"\bcannot\b", "can"), (r"\bcan't\b", "can"), (r"\bwon't\b", "will"), (r"\bwill not\b", "will"),
    (r"\bhas not\b", "has"), (r"\bhasn't\b", "has"), (r"\bhave not\b", "have"), (r"\bhaven't\b", "have"),
    (r"\bnever\b", "always"), (r"\bno longer\b", "still"), (r"\bwithout\b", "with"), (r"\bno\b", "some"),
    (r"\bdenied\b", "confirmed"), (r"\bdeclined\b", "agreed"), (r"\brejected\b", "approved"),
    (r"\bdeferred\b", "approved"), (r"\bcarried\b", "lost"), (r"\blost\b", "carried"),
    (r"\bwill\b", "will not"), (r"\bis\b", "is not"), (r"\bwas\b", "was not"), (r"\bare\b", "are not"),
    (r"\bcan\b", "cannot"), (r"\bshould\b", "should not"), (r"\bhas\b", "has not"), (r"\bhave\b", "have not"),
    (r"\bagreed\b", "did not agree"), (r"\bapproved\b", "rejected"), (r"\bsupports?\b", "opposes"),
]
_VI_NEG_REMOVE = [(W + r"(không|chưa|chẳng)\s+(?!(?:gian|khí|phải là|những|chỉ)\b)", "")]
_VI_NEG_ADD = [(W + r"đã\s+", "chưa "), (W + r"sẽ\s+", "sẽ không "), (W + r"cần\s+", "không cần "),
               (W + r"nên\s+", "không nên "), (W + r"muốn\s+", "không muốn "), (W + r"được\s+", "không được "),
               (W + r"có thể\s+", "không thể "), (W + r"là\s+", "không phải là "), (W + r"có\s+", "không có "),
               (W + r"đồng ý", "không đồng ý"), (W + r"thích\s+", "không thích ")]


def mut_negation(b, ctx, rng):
    t = b["text"]
    if b["lang"] == "vi":
        low = t.lower()
        if re.search(r"(hay|hoặc|có)\s+\S+(\s+\S+)?\s+không\b\s*[?.]?$", low):  # yes/no question form
            return None
        new = _apply_first(_VI_NEG_REMOVE, t)
        if new and new != t:
            return new
        return _apply_first(_VI_NEG_ADD, t)
    return _apply_first(_EN_NEG, t)


# --------------------------------------------------------------------------- hedges

_EN_HEDGE_DROP = [(r"\b(?:may|might|could) (?:possibly )?be\b", "is"), (r"\b(?:may|might) have\b", "has"),
                  (r"\b(?:may|might|could)\s+(\w+)", r"will \1"), (r"\b(?:maybe|perhaps|possibly|probably|likely),?\s+", ""),
                  (r"\b(?:roughly|about|around|approximately|some)\s+(?=\d)", ""),
                  (r"\b(?:seems|appears) to be\b", "is"), (r"\b(?:it seems|apparently),?\s+", ""),
                  (r"\b(?:i think|i guess|i believe)\s+", ""), (r"\b(?:thinks|believes|feels|suggested) that\b", "stated that"),
                  (r"\bnot sure (?:whether|if)\b", "confirmed that"), (r"\bconsidering\b", "decided on"),
                  (r"\bproposed\b", "decided"), (r"\bsuggested\b", "decided"), (r"\bplans? to\b", "will")]
_VI_HEDGE_DROP = [(W + r"(?:có lẽ|có thể là|có khả năng|hình như|dường như|chắc là|chắc|khả năng là|nghe nói|"
                   r"đâu đó|tầm|khoảng|chừng|gần|hơn)\s+", ""),
                  (W + r"có thể\s+", "sẽ "), (W + r"(?:nghĩ là|nghĩ rằng|cho rằng|cho là|thấy là|thấy rằng)\s+", "khẳng định "),
                  (W + r"(?:đề xuất|đề nghị|gợi ý)\s+", "quyết định "), (W + r"dự kiến\s+", "đã "),
                  (W + r"(?:định|tính|muốn)\s+", "đã ")]
_EN_HEDGE_ADD = [(r"^(.)", r"Possibly, \1")]
_VI_HEDGE_ADD = [(r"^(.)", r"Có lẽ \1")]


def mut_hedge_drop(b, ctx, rng):
    rules = _VI_HEDGE_DROP if b["lang"] == "vi" else _EN_HEDGE_DROP
    new = _apply_first(rules, b["text"])
    return new[0].upper() + new[1:] if new else None


def mut_hedge_add(b, ctx, rng):
    t = b["text"]
    hedges = _VI_HEDGE_DROP if b["lang"] == "vi" else _EN_HEDGE_DROP
    if any(re.search(p, t, re.IGNORECASE) for p, _ in hedges):
        return None  # already hedged: adding one is not a clean certainty change
    if not re.match(r"^[A-ZĐÀ-Ỹa-zà-ỹ]", t):
        return None
    if b["lang"] == "vi":
        return "Có lẽ " + t[0].lower() + t[1:] if not re.match(r"^S\d|^[A-ZĐ][a-zà-ỹ]+\s+[A-ZĐ]", t) else "Có lẽ " + t
    return "Possibly, " + (t[0].lower() + t[1:] if not re.match(r"^(S\d|[A-Z][a-z]+ [A-Z]|[A-Z]{2,})", t) else t)


# --------------------------------------------------------------------------- quantifiers

_EN_Q = [(r"\ball\b", "some"), (r"\beveryone\b", "a few people"), (r"\bevery\b", "some"), (r"\bmost\b", "a few"),
         (r"\bmany\b", "a few"), (r"\bseveral\b", "all"), (r"\bsome\b", "all"), (r"\ba few\b", "most"),
         (r"\balways\b", "sometimes"), (r"\bsometimes\b", "always"), (r"\boften\b", "rarely"), (r"\busually\b", "rarely"),
         (r"\brarely\b", "often"), (r"\bnone\b", "most"), (r"\bboth\b", "neither of the"), (r"\bonly\b", "also"),
         (r"\bunanimously\b", "narrowly"), (r"\bmajority\b", "minority")]
_VI_Q = [(W + r"tất cả" + E, "một số"), (W + r"mọi người" + E, "một vài người"), (W + r"mọi" + E, "một vài"),
         (W + r"hầu hết" + E, "một vài"), (W + r"đa số" + E, "thiểu số"), (W + r"nhiều" + E, "ít"),
         (W + r"một số" + E, "tất cả"), (W + r"một vài" + E, "tất cả"), (W + r"vài" + E, "rất nhiều"),
         (W + r"luôn luôn" + E, "thỉnh thoảng"), (W + r"luôn" + E, "thỉnh thoảng"), (W + r"thường xuyên" + E, "hiếm khi"),
         (W + r"thường" + E, "hiếm khi"), (W + r"thỉnh thoảng" + E, "luôn luôn"), (W + r"ít" + E, "nhiều"),
         (W + r"cả hai" + E, "chỉ một"), (W + r"các" + E, "một vài"), (W + r"những" + E, "một vài"),
         (W + r"chỉ" + E, "cả"), (W + r"toàn bộ" + E, "một phần")]


def mut_quantifier(b, ctx, rng):
    return _apply_first(_VI_Q if b["lang"] == "vi" else _EN_Q, b["text"])


# --------------------------------------------------------------------------- temporal

_EN_DAYS = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"]
_EN_MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
              "November", "December"]
_VI_DAYS = ["thứ Hai", "thứ Ba", "thứ Tư", "thứ Năm", "thứ Sáu", "thứ Bảy", "Chủ nhật"]
_EN_T = [(r"\bnext week\b", "this week"), (r"\bthis week\b", "next week"), (r"\btomorrow\b", "today"),
         (r"\btoday\b", "yesterday"), (r"\byesterday\b", "last week"), (r"\bnext month\b", "this month"),
         (r"\blast week\b", "next week"), (r"\bnext year\b", "this year"), (r"\bthis morning\b", "this evening"),
         (r"\bwill be\b", "has already been"), (r"\bwill\b", "has already"), (r"\bplans? to\b", "already"),
         (r"\bhas already\b", "will"), (r"\balready\b", "not yet"), (r"\bbefore\b", "after"), (r"\bafter\b", "before"),
         (r"\bused to\b", "currently"), (r"\bformer\b", "current"), (r"\bpreviously\b", "currently"),
         (r"\bstill\b", "no longer"), (r"\bcurrently\b", "previously"), (r"\b(\d{1,2})(:\d\d)?\s*(am|pm)\b", None)]
_VI_T = [(W + r"tuần sau" + E, "tuần này"), (W + r"tuần tới" + E, "tuần này"), (W + r"tuần này" + E, "tuần sau"),
         (W + r"tuần trước" + E, "tuần sau"), (W + r"ngày mai" + E, "hôm nay"), (W + r"hôm nay" + E, "hôm qua"),
         (W + r"hôm qua" + E, "tuần trước"), (W + r"tháng sau" + E, "tháng này"), (W + r"tháng tới" + E, "tháng này"),
         (W + r"tháng này" + E, "tháng sau"), (W + r"năm sau" + E, "năm nay"), (W + r"năm nay" + E, "năm ngoái"),
         (W + r"sáng nay" + E, "tối nay"), (W + r"tối nay" + E, "sáng mai"), (W + r"sắp" + E, "đã"),
         (W + r"sẽ" + E, "đã"), (W + r"đã" + E, "sẽ"), (W + r"đang" + E, "đã từng"), (W + r"trước khi" + E, "sau khi"),
         (W + r"sau khi" + E, "trước khi"), (W + r"vẫn" + E, "không còn"), (W + r"từng" + E, "đang"),
         (W + r"rồi" + E, "sau")]


def _shift_day(t: str, names: list[str], k: int = 2) -> str | None:
    for i, d in enumerate(names):
        m = re.search(W + re.escape(d) + E, t, flags=re.IGNORECASE)
        if m:
            return t[: m.start()] + _keepcase(m.group(0), names[(i + k) % len(names)]) + t[m.end():]
    return None


def mut_temporal(b, ctx, rng):
    t, lang = b["text"], b["lang"]
    # explicit dates / times first: dd/mm, "ngày 15", "15h", "3pm"
    m = re.search(r"(?<!\d)(\d{1,2})/(\d{1,2})(?!\d)", t)
    if m:
        d = (int(m.group(1)) + 3 - 1) % 28 + 1
        return t[: m.start(1)] + str(d) + t[m.end(1):]
    m = re.search(r"(?<!\d)(\d{1,2})\s*(h|giờ|g|am|pm|:\d\d)", t, flags=re.IGNORECASE)
    if m:
        h = (int(m.group(1)) + 2) % 24 or 1
        return t[: m.start(1)] + str(h) + t[m.end(1):]
    m = re.search(r"\b(ngày|day)\s+(\d{1,2})\b", t, flags=re.IGNORECASE)
    if m:
        return t[: m.start(2)] + str((int(m.group(2)) + 3 - 1) % 28 + 1) + t[m.end(2):]
    for names in ((_VI_DAYS,) if lang == "vi" else (_EN_DAYS, _EN_MONTHS)):
        new = _shift_day(t, list(names))
        if new:
            return new
    rules = _VI_T if lang == "vi" else [r for r in _EN_T if r[1] is not None]
    return _apply_first(rules, t)


# --------------------------------------------------------------------------- speaker misattribution


def mut_speaker(b, ctx, rng):
    t = b["text"]
    names = [n for n in ctx["speakers"] if n]
    present = [n for n in names if re.search(W + re.escape(n) + E, t)]
    if not present or len(names) < 2:
        return None
    who = present[0]
    others = [n for n in names if n != who and n not in present]
    if not others:
        return None
    return re.sub(W + re.escape(who) + E, rng.choice(sorted(others)), t, count=1)


# --------------------------------------------------------------------------- overgeneralisation

_EN_GEN = [(r"\blast (monday|tuesday|wednesday|thursday|friday|saturday|sunday|week|weekend|night|month)\b", "regularly"),
           (r"\byesterday\b", "every day"), (r"\bthis morning\b", "every morning"), (r"\bthis time\b", "every time"),
           (r"\bonce\b", "repeatedly"), (r"\bon one occasion\b", "repeatedly"), (r"\bsometimes\b", "always"),
           (r"\bin this case\b", "in every case"), (r"\bfor this project\b", "for all projects"),
           (r"^(S\d+|[A-Z][a-z]+(?: [A-Z][a-z]+)?) (said|noted|thinks|believes|suggested|mentioned|explained|"
            r"stated|feels|agreed|proposed|asked|raised|reported)\b", r"Everyone \2"),
           (r"^(The officer|The Chair|The chair|Cr [A-Z][a-z]+|Councillor [A-Z][a-z]+) (\w+)", r"All members \2")]
_VI_GEN = [(W + r"hôm qua" + E, "hằng ngày"), (W + r"hôm nay" + E, "ngày nào cũng"), (W + r"lần này" + E, "lần nào cũng"),
           (W + r"một lần" + E, "nhiều lần"), (W + r"tuần này" + E, "tuần nào cũng"), (W + r"đôi khi" + E, "luôn luôn"),
           (W + r"thỉnh thoảng" + E, "luôn luôn"), (W + r"trong trường hợp này" + E, "trong mọi trường hợp"),
           (r"^(S\d+|[A-ZĐ][a-zà-ỹ]+(?: [A-ZĐ][a-zà-ỹ]+)?) (cho rằng|nói|cho biết|nghĩ|thấy|đề xuất|đề nghị|chia sẻ|"
            r"giải thích|nhận xét|đồng ý|muốn|hỏi|kể|nhắc)", r"Mọi người đều \2")]


def mut_overgeneralise(b, ctx, rng):
    return _apply_first(_VI_GEN if b["lang"] == "vi" else _EN_GEN, b["text"])


# --------------------------------------------------------------------------- fabrications

FABRICATIONS = {
    "vi": [
        "Nhóm đã quyết định tăng ngân sách dự án lên 500 triệu đồng.",
        "Cuộc họp thống nhất hoãn ngày ra mắt sang tháng Ba năm sau.",
        "Ban giám đốc đã phê duyệt việc tuyển thêm năm nhân sự mới.",
        "Mọi người đồng ý chuyển toàn bộ hệ thống sang máy chủ ở Singapore.",
        "Khách hàng yêu cầu bồi thường 20% giá trị hợp đồng.",
        "Nhóm sẽ ngừng hỗ trợ phiên bản cũ từ thứ Hai tuần sau.",
        "Kế toán xác nhận công ty đã lỗ 2 tỷ đồng trong quý này.",
        "Đối tác đã ký hợp đồng độc quyền trong ba năm.",
    ],
    "en": [
        "The team decided to increase the project budget to $500,000.",
        "The launch was postponed until March next year.",
        "Management approved hiring five additional engineers.",
        "Everyone agreed to migrate all servers to the Singapore region.",
        "The client demanded a 20% refund on the contract.",
        "Support for the old version will end next Monday.",
        "Finance confirmed a loss of $2 million this quarter.",
        "The partner signed a three-year exclusive agreement.",
    ],
}


def fabrication(lang: str, transcript_folded: str, rng: random.Random, k: int = 1) -> list[str]:
    pool = [s for s in FABRICATIONS[lang] if fold(s)[:25] not in transcript_folded]
    return rng.sample(pool, k) if len(pool) >= k else []


MUTATORS = {
    "number": mut_number,
    "entity": mut_entity,
    "negation": mut_negation,
    "hedge_drop": mut_hedge_drop,
    "hedge_add": mut_hedge_add,
    "quantifier": mut_quantifier,
    "temporal": mut_temporal,
    "speaker": mut_speaker,
    "overgeneralise": mut_overgeneralise,
}
INSERTS = ("fabricate", "cluster_insert")
ALL_TYPES = list(MUTATORS) + list(INSERTS)
