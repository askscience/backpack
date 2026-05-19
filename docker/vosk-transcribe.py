#!/usr/bin/env python3
import sys
import json
import os
from vosk import Model, KaldiRecognizer
import wave

model_path = os.environ.get("VOSK_MODEL_PATH", "/opt/vosk-model")

if not os.path.exists(model_path):
    print(f"Error: Vosk model not found at {model_path}", file=sys.stderr)
    print("")
    sys.exit(1)

try:
    model = Model(model_path)
except Exception as e:
    print(f"Error loading Vosk model: {e}", file=sys.stderr)
    print("")
    sys.exit(1)

wf = wave.open(sys.argv[1], "rb")
if wf.getframerate() != 16000:
    print("Error: Audio must be 16kHz mono WAV", file=sys.stderr)
    sys.exit(1)

rec = KaldiRecognizer(model, wf.getframerate())
rec.SetWords(False)

while True:
    data = wf.readframes(4000)
    if len(data) == 0:
        break
    rec.AcceptWaveform(data)

result = json.loads(rec.FinalResult())
print(result.get("text", ""))
