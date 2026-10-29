#!/usr/bin/env python3
"""API publik help.deca.co.id — buat tiket di Zammad (server-side credential)."""
import base64, json, os, urllib.request, urllib.error, urllib.parse
import cif
from http.server import BaseHTTPRequestHandler, HTTPServer

Z = os.environ.get("ZAMMAD_URL", "http://127.0.0.1:8080")
U = os.environ.get("ZAMMAD_USER", "admin@deca.co.id")
P = os.environ.get("ZAMMAD_PASS", "")
AUTH = "Basic " + base64.b64encode(f"{U}:{P}".encode()).decode()

def zm(path, payload=None, method="GET"):
    data = json.dumps(payload).encode() if payload else None
    req = urllib.request.Request(Z + path, data=data, method=method,
        headers={"Authorization": AUTH, "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=25) as r:
        return json.loads(r.read().decode() or "{}")


def lark_create(ticket_no, merchant, pelapor, kontak, kategori, deskripsi, wa):
    tok = os.environ.get("LARK_TOKEN", "")
    app = os.environ.get("LARK_APP", "")
    tbl = os.environ.get("LARK_TABLE", "")
    if not (tok and app and tbl):
        return None
    fields = {
        "Ticket ID": ticket_no,
        "Judul": f"[{kategori}] {merchant or pelapor}",
        "Deskripsi": deskripsi,
        "Kanal": "Website (help.deca.co.id)",
        "Pelapor": pelapor,
        "Tipe Pelapor": "Merchant",
        "Kontak": kontak + ((" / " + wa) if wa else ""),
        "Kategori": kategori,
        "Severity": "S3 - Medium",
        "Status": "Baru",
        "Catatan": "Dibuat otomatis dari portal bantuan",
    }
    body = json.dumps({"fields": fields}).encode()
    req = urllib.request.Request(
        f"https://open.larksuite.com/open-apis/bitable/v1/apps/{app}/tables/{tbl}/records",
        data=body, method="POST",
        headers={"Authorization": "Bearer " + tok, "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return json.loads(r.read().decode()).get("data", {}).get("record", {}).get("record_id")
    except Exception:
        return None


# ---------- Laporan Penanganan Pengaduan (BI / PADG 32-2026) ----------
CS_APP = "Q9HlbQtAOaQ6gKs9Uj1l7xoJgze"
CS_LAPORAN = "tblC2XFw3fKNMRx5"
REF = {
  "company_dob": "reczz28SMJoeYF78",
  "status_investigasi": "reczz28SN9TgSNoa",
  "unit_kp": "reczz28SNMPafrLe",
  "jenis_ktp": "reczz28SNHvZ7gzs",
}

def cs_report(ticket_no, merchant, pelapor, email, kategori, deskripsi, nominal=0, produk=None, aktivitas=None):
    tok = os.environ.get("LARK_TOKEN", "")
    if not tok:
        return None
    fields = {
        "Company": [REF["company_dob"]],
        "Produk/Layanan/Kanal/Instrumen": f"DECA Apps / {kategori} / Website Form",
        "Aktivitas Transaksi": kategori,
        "Penyebab": "",
        "Nilai Pengaduan": float(nominal or 0),
        "Status Pengaduan": [REF["status_investigasi"]],
        "Tanggal Penyelesaian Pengaduan": "",
        "Jenis Identitas Konsumen": [REF["jenis_ktp"]],
        "Nomor Identitas Konsumen": 0,
        "Nama Konsumen": pelapor,
        "Email Konsumen": email,
        "Unit Penerima Konsumen": [REF["unit_kp"]],
        "KC LN": "",
        "Resume Kronologi Aduan": f"Merchant: {merchant}\nTiket: {ticket_no}\n\n{deskripsi}",
    }
    body = json.dumps({"fields": fields}).encode()
    req = urllib.request.Request(
        f"https://open.larksuite.com/open-apis/bitable/v1/apps/{CS_APP}/tables/{CS_LAPORAN}/records",
        data=body, method="POST",
        headers={"Authorization": "Bearer " + tok, "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return json.loads(r.read().decode()).get("data", {}).get("record", {}).get("record_id")
    except Exception as e:
        return "ERR:" + str(e)[:60]

class H(BaseHTTPRequestHandler):
    def _json(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_OPTIONS(self):
        self._json(200, {"ok": True})

    def _merchant(self):
        q = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        prof = cif.lookup(q.get("email",[""])[0], q.get("nmid",[""])[0], q.get("phone",[""])[0])
        self._json(200, {"ok": True, "cif_configured": cif.configured(), "merchant": prof})

    def do_POST(self):
        if not self.path.startswith("/api/ticket"):
            return self._json(404, {"error": "not found"})
        n = int(self.headers.get("Content-Length", 0))
        try:
            body = json.loads(self.rfile.read(n) or b"{}")
        except Exception:
            return self._json(400, {"error": "payload tidak valid"})
        merchant = (body.get("merchant") or "").strip()
        name = (body.get("name") or "").strip()
        contact = (body.get("email") or body.get("contact") or "").strip()
        cat = (body.get("category") or "Lainnya").strip()
        desc = (body.get("body") or "").strip()
        if not (name and contact and desc):
            return self._json(400, {"error": "nama, kontak, dan deskripsi wajib"})
        try:
            # 1) pastikan customer ada (Zammad tidak auto-create dari email)
            cust_id = None
            if "@" in contact:
                sr = zm("/api/v1/users/search?query=" + urllib.parse.quote(contact) + "&limit=5")
                hits = sr if isinstance(sr, list) else sr.get("assets", {}).get("User", {})
                for h in (hits if isinstance(hits, list) else list(hits.values())):
                    if isinstance(h, dict) and h.get("email") == contact:
                        cust_id = h.get("id")
                        break
                if not cust_id:
                    parts = name.split()
                    try:
                        nu = zm("/api/v1/users", {
                            "firstname": parts[0],
                            "lastname": " ".join(parts[1:]) or contact.split("@")[0],
                            "email": contact,
                            "verified": True,
                        }, "POST")
                        cust_id = nu.get("id")
                    except urllib.error.HTTPError:
                        cust_id = None
            payload = {
                "title": f"[{cat}] {merchant or name} — {desc[:60]}",
                "group": "Users",
                "article": {"subject": f"[{cat}] Laporan dari help.deca.co.id",
                            "body": f"Merchant: {merchant or '-'}\nPelapor: {name}\nEmail terdaftar: {contact}\nWhatsApp: {body.get('wa') or '-'}\n\n{desc}",
                            "type": "web", "internal": False},
                "tags": "help-portal," + cat.lower().replace(" ", "-") + ("," + merchant.lower().replace(" ", "-") if merchant else ""),
            }
            if cust_id:
                payload["customer_id"] = cust_id
            else:
                payload["customer_id"] = 1  # fallback: admin sebagai pelapor, kontak ada di body
            t = zm("/api/v1/tickets", payload, "POST")
            no = f"DECA-{t.get('number')}"
            rid = lark_create(no, merchant, name, contact, cat, desc, body.get("wa") or "")
            csid = cs_report(no, merchant, name, contact, cat, desc, body.get("nominal") or 0, body.get("produk"), body.get("aktivitas"))
            self._json(200, {"ticket": no, "id": t.get("id"), "lark": rid or "gagal", "laporan_bi": csid or "gagal"})
        except urllib.error.HTTPError as e:
            self._json(502, {"error": f"zammad {e.code}"})
        except Exception as e:
            self._json(500, {"error": str(e)})

    def log_message(self, *a):
        pass

if __name__ == "__main__":
    HTTPServer(("127.0.0.1", 3002), H).serve_forever()
