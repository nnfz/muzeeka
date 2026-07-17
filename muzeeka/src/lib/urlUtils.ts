/** Normalize pasted text into a fetchable URL when possible. */
export function normalizeMediaUrl(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed) return null;

  if (/^https?:\/\//i.test(trimmed)) {
    return trimmed;
  }

  if (/^www\./i.test(trimmed)) {
    return `https://${trimmed}`;
  }

  return null;
}

/** Quick client-side check before calling the backend. */
export function looksLikeMediaUrl(text: string): boolean {
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
    'spotify.com', 'nicovideo.jp', 'bilibili.com',
  ];

  return hosts.some((host) => lower.includes(host));
}