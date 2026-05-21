async function webauthnRegister(token) {
  const startRes = await fetch(`${API_BASE_URL}/api/webauthn/register/start`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ token }),
  });
  if (!startRes.ok) throw new Error(await startRes.text());
  const { challenge_id, public_key } = await startRes.json();

  public_key.publicKey.challenge = base64ToBuffer(public_key.publicKey.challenge);
  public_key.publicKey.user.id = base64ToBuffer(public_key.publicKey.user.id);
  if (public_key.publicKey.excludeCredentials) {
    for (const cred of public_key.publicKey.excludeCredentials) {
      cred.id = base64ToBuffer(cred.id);
    }
  }

  const credential = await navigator.credentials.create({ publicKey: public_key.publicKey });

  const finishBody = {
    challenge_id,
    credential: credentialToJSON(credential),
  };

  const finishRes = await fetch(`${API_BASE_URL}/api/webauthn/register/finish`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(finishBody),
  });
  if (!finishRes.ok) throw new Error(await finishRes.text());
  return finishRes.json();
}

async function webauthnAuthenticate() {
  const startRes = await fetch(`${API_BASE_URL}/api/webauthn/auth/start`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: '{}',
  });
  if (!startRes.ok) throw new Error(await startRes.text());
  const { challenge_id, public_key } = await startRes.json();

  public_key.publicKey.challenge = base64ToBuffer(public_key.publicKey.challenge);
  if (public_key.publicKey.allowCredentials) {
    for (const cred of public_key.publicKey.allowCredentials) {
      cred.id = base64ToBuffer(cred.id);
    }
  }

  const assertion = await navigator.credentials.get({ publicKey: public_key.publicKey });

  const finishBody = {
    challenge_id,
    credential: assertionToJSON(assertion),
  };

  const finishRes = await fetch(`${API_BASE_URL}/api/webauthn/auth/finish`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(finishBody),
  });
  if (!finishRes.ok) throw new Error(await finishRes.text());
  return finishRes.json();
}

async function getSpaceShares(token) {
  const res = await fetch(`${API_BASE_URL}/space/shares?token=${encodeURIComponent(token)}`);
  if (!res.ok) throw new Error(await res.text());
  return res.json();
}

async function revokeAllSpaceShares(token) {
  const res = await fetch(`${API_BASE_URL}/space/shares/revoke-all?token=${encodeURIComponent(token)}`, { method: 'POST' });
  if (!res.ok) throw new Error(await res.text());
  return res.json();
}

function base64ToBuffer(b64) {
  const bin = atob(b64.replace(/-/g, '+').replace(/_/g, '/'));
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes.buffer;
}

function bufferToBase64(buffer) {
  const bytes = new Uint8Array(buffer);
  let bin = '';
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

function credentialToJSON(cred) {
  return {
    id: cred.id,
    rawId: bufferToBase64(cred.rawId),
    type: cred.type,
    response: {
      clientDataJSON: bufferToBase64(cred.response.clientDataJSON),
      attestationObject: bufferToBase64(cred.response.attestationObject),
    },
  };
}

function assertionToJSON(assertion) {
  return {
    id: assertion.id,
    rawId: bufferToBase64(assertion.rawId),
    type: assertion.type,
    response: {
      clientDataJSON: bufferToBase64(assertion.response.clientDataJSON),
      authenticatorData: bufferToBase64(assertion.response.authenticatorData),
      signature: bufferToBase64(assertion.response.signature),
      userHandle: assertion.response.userHandle ? bufferToBase64(assertion.response.userHandle) : null,
    },
  };
}
