import os
import sys
import asyncio
import json
import uuid
import time
import logging
import imaplib
import smtplib
import email
from email.mime.text import MIMEText
from email.mime.multipart import MIMEMultipart
from dotenv import load_dotenv

logging.basicConfig(
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s", level=logging.INFO
)
logger = logging.getLogger("email_adapter")

load_dotenv()
BUS_PORT = int(os.getenv("BUS_PORT", 5000))
EMAIL_IMAP_SERVER = os.getenv("EMAIL_IMAP_SERVER", "").strip("\"'")
EMAIL_SMTP_SERVER = os.getenv("EMAIL_SMTP_SERVER", "").strip("\"'")
EMAIL_IMAP_PORT = int(os.getenv("EMAIL_IMAP_PORT", 993))
EMAIL_SMTP_PORT = int(os.getenv("EMAIL_SMTP_PORT", 587))
EMAIL_USER = os.getenv("EMAIL_USER", "").strip("\"'")
EMAIL_PASSWORD = os.getenv("EMAIL_PASSWORD", "").strip("\"'")
EMAIL_ALLOWED_SENDERS = set(s.strip() for s in os.getenv("EMAIL_ALLOWED_SENDERS", "").split(",") if s.strip())
EMAIL_POLL_INTERVAL = int(os.getenv("EMAIL_POLL_INTERVAL", 30))

class BusConnection:
    def __init__(self, reader, writer):
        self.reader = reader
        self.writer = writer

    @classmethod
    async def connect(cls):
        reader, writer = await asyncio.open_connection("127.0.0.1", BUS_PORT)
        return cls(reader, writer)

    async def close(self):
        try:
            self.writer.close()
            await self.writer.wait_closed()
        except Exception:
            pass

async def send_intent_to_bus(content, sender):
    try:
        bus = await BusConnection.connect()
    except Exception as e:
        logger.error(f"Failed to connect to Event Bus: {e}")
        return "❌ Error: Core engine is offline."

    req = {
        "jsonrpc": "2.0",
        "method": "intent.submit",
        "params": {
            "page_id": f"email-{sender.replace('@', '_')}",
            "channel_id": "email",
            "user_id": f"email-{sender}",
            "content": content,
            "attachments": [],
            "metadata": {},
            "timestamp": int(time.time() * 1000),
            "priority": "normal"
        },
        "id": str(uuid.uuid4())
    }

    text_buffer = ""
    try:
        bus.writer.write((json.dumps(req) + "\n").encode())
        await bus.writer.drain()

        while True:
            line = await bus.reader.readline()
            if not line:
                break
            msg = json.loads(line.decode().strip())
            if msg.get("method") == "response.token":
                text_buffer += msg["params"]["token"]
            elif "result" in msg:
                break
        return text_buffer
    except Exception as e:
        logger.error(f"Error in email-bus stream: {e}")
        return "❌ Error communicating with the core engine."
    finally:
        await bus.close()

def send_reply(to_addr, original_subject, body):
    if not EMAIL_SMTP_SERVER or not EMAIL_USER or not EMAIL_PASSWORD:
        logger.warning("SMTP configuration is missing. Cannot send email replies.")
        return

    subject = f"Re: {original_subject}" if not original_subject.startswith("Re:") else original_subject
    msg = MIMEMultipart()
    msg["Subject"] = subject
    msg["From"] = EMAIL_USER
    msg["To"] = to_addr

    msg.attach(MIMEText(body, "plain"))

    try:
        with smtplib.SMTP(EMAIL_SMTP_SERVER, EMAIL_SMTP_PORT) as smtp:
            smtp.starttls()
            smtp.login(EMAIL_USER, EMAIL_PASSWORD)
            smtp.sendmail(EMAIL_USER, to_addr, msg.as_string())
        logger.info(f"Replied to {to_addr}")
    except Exception as e:
        logger.error(f"Failed to send email reply to {to_addr}: {e}")

def process_email(msg):
    sender = email.utils.parseaddr(msg["From"])[1]
    subject = msg.get("Subject", "(no subject)")
    
    if EMAIL_ALLOWED_SENDERS and sender not in EMAIL_ALLOWED_SENDERS:
        logger.warning(f"Sender {sender} is not authorized. Dropping email.")
        return None, None, None

    body = ""
    if msg.is_multipart():
        for part in msg.walk():
            if part.get_content_type() == "text/plain":
                body = part.get_payload(decode=True).decode("utf-8", errors="replace")
                break
    else:
        body = msg.get_payload(decode=True).decode("utf-8", errors="replace")

    return sender, subject, body.strip()

async def poll_emails():
    if not EMAIL_IMAP_SERVER or not EMAIL_USER or not EMAIL_PASSWORD:
        logger.error("IMAP configuration is incomplete. Email adapter cannot poll.")
        return

    logger.info(f"Starting email adapter polling every {EMAIL_POLL_INTERVAL} seconds...")
    while True:
        try:
            loop = asyncio.get_running_loop()
            await loop.run_in_executor(None, sync_poll)
        except Exception as e:
            logger.error(f"Error during email poll cycle: {e}")
        await asyncio.sleep(EMAIL_POLL_INTERVAL)

def sync_poll():
    try:
        imap = imaplib.IMAP4_SSL(EMAIL_IMAP_SERVER, EMAIL_IMAP_PORT)
        imap.login(EMAIL_USER, EMAIL_PASSWORD)
        imap.select("INBOX")
        
        status, messages = imap.search(None, "UNSEEN")
        if status != "OK":
            return
            
        for num in messages[0].split():
            status, data = imap.fetch(num, "(RFC822)")
            if status != "OK":
                continue
            
            raw_email = data[0][1]
            msg = email.message_from_bytes(raw_email)
            sender, subject, body = process_email(msg)
            
            if sender and body:
                logger.info(f"Processing email from {sender}: {subject}")
                # Create a temporary event loop to run async intent
                temp_loop = asyncio.new_event_loop()
                try:
                    reply_body = temp_loop.run_until_complete(send_intent_to_bus(body, sender))
                finally:
                    temp_loop.close()
                send_reply(sender, subject, reply_body)
                
            imap.store(num, "+FLAGS", "\\Seen")
            
        imap.close()
        imap.logout()
    except Exception as e:
        logger.error(f"IMAP session error: {e}")

if __name__ == "__main__":
    asyncio.run(poll_emails())
