/** Normalize pasted text into a fetchable URL when possible. */
export function normalizeMediaUrl(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed) return null;

  if (/^https?:\/\//i.test(trimmed)) {
    return trimmed;
  }

  // spotify:track:xxx → https://open.spotify.com/track/xxx
  const spotifyUri = trimmed.match(
    /^spotify:(track|album|playlist|artist|episode|show):([a-zA-Z0-9]+)/i
  );
  if (spotifyUri) {
    return `https://open.spotify.com/${spotifyUri[1].toLowerCase()}/${spotifyUri[2]}`;
  }

  if (/^www\./i.test(trimmed)) {
    return `https://${trimmed}`;
  }

  return null;
}

/** Quick client-side check before calling the backend. */
export function looksLikeMediaUrl(text: string): boolean {
  const trimmed = text.trim();
  if (/^spotify:/i.test(trimmed)) return true;

  const url = normalizeMediaUrl(text);
  if (!url) return false;

  const lower = url.toLowerCase();
  const hosts = [
    'youtube.com', 'youtu.be', 'music.youtube.com',
    'soundcloud.com', 'bandcamp.com', 'vimeo.com',
    'twitch.tv', 'tiktok.com', 'instagram.com',
    'twitter.com', 'x.com', 'facebook.com',
    'vk.com', 'vk.ru', 'm.vk.com', 'm.vk.ru',
    'rutube.ru', 'dailymotion.com',
    'mixcloud.com', 'audiomack.com', 'deezer.com',
    'spotify.com', 'spotify.link', 'spoti.fi',
    'nicovideo.jp', 'bilibili.com',
  ];

  return hosts.some((host) => lower.includes(host));
}