import json
import os
import smtplib
import ssl
import sys
from pathlib import Path
from email.message import EmailMessage
from email.utils import formatdate

def send(row):
    sender = os.environ.get('SMTP_FROM', 'cs@deca.co.id')
    message = EmailMessage()
    message['From'] = sender
    message['To'] = row['email']
    message['Date'] = formatdate(localtime=True)
    message['Message-ID'] = f"<deca-{row['event']}@{sender.rsplit('@', 1)[1]}>"
    title = 'Pengaduan diterima' if row['event'].startswith('receipt-') else 'Perubahan status'
    message['Subject'] = f"{title} • {row['number']}"
    message.set_content(f"Halo,\n\nNomor tiket: {row['number']}\n{row['body']}\n\nSalam,\nTim CS DECA")
    context = ssl.create_default_context()
    port = int(os.environ.get('SMTP_PORT', '465'))
    options = dict(host=os.environ['SMTP_HOST'], port=port, timeout=20)
    connection = smtplib.SMTP_SSL(context=context, **options) if port == 465 else smtplib.SMTP(**options)
    with connection as smtp:
        if port != 465:
            smtp.starttls(context=context)
        if os.environ.get('SMTP_USERNAME'):
            password = os.environ.get('SMTP_PASSWORD')
            if not password:
                password = Path(os.environ['SMTP_PASSWORD_FILE']).read_text().strip()
            if os.environ['SMTP_HOST'] == 'smtp.gmail.com':
                password = password.replace(' ', '')
            smtp.login(os.environ['SMTP_USERNAME'], password)
        smtp.send_message(message, from_addr=sender, to_addrs=[row['email']])

if __name__ == '__main__':
    try:
        send(json.load(sys.stdin))
    except Exception as exc:
        print('SMTP delivery:', type(exc).__name__, getattr(exc, 'smtp_code', ''), file=sys.stderr)
        sys.exit(1)
