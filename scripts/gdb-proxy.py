#!/usr/bin/env python3
"""
scripts/gdb-proxy.py
====================
Proxy GDB RSP : TCP (GDB) <-> Serial (Mega Everdrive Pro USB)

Le stub 68k dans la ROM communique via $A130E2 (SSF mapper USB register).
Ce proxy relaie les paquets RSP entre m68k-elf-gdb et le hardware.

Usage:
  python3 scripts/gdb-proxy.py [options]
  python3 scripts/gdb-proxy.py --port /dev/everdrive --gdb-port 2345 --baud 115200

Prérequis:
  pip install pyserial
"""

import argparse
import socket
import serial
import threading
import sys
import signal
import time
import logging

logging.basicConfig(
    level=logging.INFO,
    format='\033[36m[gdb-proxy]\033[0m %(message)s'
)
log = logging.getLogger(__name__)


def parse_args():
    p = argparse.ArgumentParser(description="GDB RSP proxy: TCP <-> Mega Everdrive Pro USB")
    p.add_argument("--port", default="/dev/everdrive",
                   help="Port série ED Pro (défaut: /dev/everdrive)")
    p.add_argument("--baud", type=int, default=115200,
                   help="Baud rate (défaut: 115200)")
    p.add_argument("--gdb-port", type=int, default=2345,
                   help="Port TCP pour GDB (défaut: 2345)")
    p.add_argument("--bind", default="127.0.0.1",
                   help="Adresse bind TCP (défaut: 127.0.0.1)")
    p.add_argument("--debug", action="store_true",
                   help="Log les paquets RSP")
    return p.parse_args()


class RSPProxy:
    def __init__(self, args):
        self.args = args
        self.ser = None
        self.conn = None
        self.running = False

    def open_serial(self):
        """Ouvre le port série ED Pro."""
        log.info(f"Connexion serie: {self.args.port} @ {self.args.baud} baud")
        self.ser = serial.Serial(
            self.args.port,
            baudrate=self.args.baud,
            timeout=1,
            bytesize=serial.EIGHTBITS,
            parity=serial.PARITY_NONE,
            stopbits=serial.STOPBITS_ONE
        )
        log.info("Port série ouvert")

    def wait_for_gdb(self):
        """Attend la connexion GDB sur TCP."""
        srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        srv.bind((self.args.bind, self.args.gdb_port))
        srv.listen(1)
        log.info(f"En attente de GDB sur {self.args.bind}:{self.args.gdb_port}")
        log.info("Connecte VS Code (F5, config 'ED Pro GDB hardware') ou :")
        log.info(f"  m68k-elf-gdb -ex 'target remote :{self.args.gdb_port}' out/rom.elf")
        self.conn, addr = srv.accept()
        self.conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        log.info(f"GDB connecté depuis {addr}")
        srv.close()

    def serial_to_tcp(self):
        """Thread : relaie les données serial → GDB TCP."""
        try:
            while self.running:
                data = self.ser.read(256)
                if data:
                    if self.args.debug:
                        log.info(f"SERIAL→TCP [{len(data)}B]: {data[:64]!r}")
                    self.conn.sendall(data)
        except (serial.SerialException, OSError) as e:
            if self.running:
                log.error(f"Erreur serial→tcp: {e}")
            self.running = False

    def tcp_to_serial(self):
        """Thread : relaie les données GDB TCP → serial."""
        try:
            while self.running:
                data = self.conn.recv(256)
                if not data:
                    log.info("GDB déconnecté")
                    break
                if self.args.debug:
                    log.info(f"TCP→SERIAL [{len(data)}B]: {data[:64]!r}")
                self.ser.write(data)
        except (OSError, serial.SerialException) as e:
            if self.running:
                log.error(f"Erreur tcp→serial: {e}")
        self.running = False

    def run(self):
        self.open_serial()
        self.wait_for_gdb()
        self.running = True

        t1 = threading.Thread(target=self.serial_to_tcp, daemon=True, name="ser2tcp")
        t2 = threading.Thread(target=self.tcp_to_serial, daemon=True, name="tcp2ser")
        t1.start()
        t2.start()

        log.info("Proxy actif — Ctrl+C pour arrêter")
        try:
            while self.running:
                time.sleep(0.1)
        except KeyboardInterrupt:
            pass
        finally:
            self.running = False
            log.info("Arrêt proxy")
            try: self.conn.close()
            except: pass
            try: self.ser.close()
            except: pass


def main():
    args = parse_args()

    # Vérification basique du port
    import os
    if not os.path.exists(args.port):
        log.error(f"Port {args.port} non trouvé")
        log.error("Ports disponibles :")
        for p in ["/dev/everdrive", "/dev/ttyACM0", "/dev/ttyUSB0"]:
            if os.path.exists(p):
                log.error(f"  {p}")
        sys.exit(1)

    proxy = RSPProxy(args)

    def on_signal(sig, frame):
        proxy.running = False
        sys.exit(0)
    signal.signal(signal.SIGTERM, on_signal)
    signal.signal(signal.SIGINT, on_signal)

    proxy.run()


if __name__ == "__main__":
    main()
