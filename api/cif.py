#!/usr/bin/env python3
"""
Adapter CIF — rumah untuk API merchant lookup.

Kontrak yang diharapkan dari CIF (boleh REST atau view DB):
GET  {CIF_BASE_URL}/merchants?email=<email>&nmid=<nmid>&phone=<hp>
Header: Authorization: Bearer {CIF_TOKEN}
Response 200:
{
  "nmid": "ID123456789",
  "nama_usaha": "Toko Kopdes2",
  "nama_pemilik": "Bayu Eka",
  "email": "kopdes2@example.com",
  "phone": "0897xxxxxxx",
  "mcc": "5411",
  "kategori_usaha": "Retail / Toko",
  "produk": "DECA Cashier",
  "status_kyc": "APPROVED",
  "jenis_identitas": "KTP",
  "nomor_identitas": "3273xxxxxxxx0001",
  "alamat": "Jl. ...",
  "kota": "Bogor",
  "provinsi": "Jawa Barat",
  "kode_wilayah": "3271",
  "limit_transaksi": 50000000,
  "last_trx": {"tanggal": "2026-10-28", "nominal": 150000, "status": "SUCCESS"}
}

Bila tidak ada kredensial (CIF_BASE_URL/CIF_TOKEN kosong), adapter memakai
mode STUB: mengembalikan data yang tersedia dari form saja, dengan
"source": "form". Aplikasi tidak pernah gagal hanya karena CIF belum ada.
"""
import json, os, urllib.request, urllib.error, urllib.parse, time

BASE = os.environ.get("CIF_BASE_URL", "").rstrip("/")
TOKEN = os.environ.get("CIF_TOKEN", "")
TIMEOUT = int(os.environ.get("CIF_TIMEOUT", "12"))
CACHE_TTL = int(os.environ.get("CIF_CACHE_TTL", "300"))
_cache = {}


def _mask(nid):
    nid = str(nid or "")
    if len(nid) <= 4:
        return nid
    return "*" * (len(nid) - 4) + nid[-4:]


def configured():
    return bool(BASE and TOKEN)


def lookup(email="", nmid="", phone=""):
    """Kembalikan dict profil merchant + meta sumber. Tidak pernah raise."""
    key = (email.strip().lower(), nmid.strip(), phone.strip())
    if key in _cache and time.time() - _cache[key][0] < CACHE_TTL:
        return _cache[key][1]

    if not configured():
        res = {"source": "form", "found": False,
               "note": "CIF belum dikonfigurasi (CIF_BASE_URL/CIF_TOKEN kosong)"}
        _cache[key] = (time.time(), res)
        return res

    q = {k: v for k, v in (("email", email), ("nmid", nmid), ("phone", phone)) if v}
    url = BASE + "/merchants?" + urllib.parse.urlencode(q)
    try:
        req = urllib.request.Request(url, headers={
            "Authorization": "Bearer " + TOKEN,
            "Accept": "application/json",
        })
        with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
            data = json.loads(r.read().decode())
        if isinstance(data, dict) and "data" in data and isinstance(data["data"], dict):
            data = data["data"]
        if not data or not (data.get("nmid") or data.get("nama_usaha")):
            res = {"source": "cif", "found": False}
        else:
            data["source"] = "cif"
            data["found"] = True
            if data.get("nomor_identitas"):
                data["nomor_identitas_masked"] = _mask(data["nomor_identitas"])
            res = data
    except urllib.error.HTTPError as e:
        res = {"source": "cif", "found": False, "error": "HTTP %s" % e.code}
    except Exception as e:
        res = {"source": "cif", "found": False, "error": type(e).__name__}

    _cache[key] = (time.time(), res)
    return res
