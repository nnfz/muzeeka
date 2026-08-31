/** Tracking / share junk that is safe to drop. Never drop YouTube `v` / `list`. */
const DROP_QUERY_PARAMS = new Set([
  'si',
  'feature',
  'pp',
  'app',
  'fbclid',
  'gclid',
  'utm_source',
  'utm_medium',
  'utm_campaign',
  'utm_content',
  'utm_term',
  'embeds_referring_euri',
  'embeds_referring_origin',
  'source_ve_path',
]);

/**
 * Normalize pasted text into a fetchable URL when possible.
 * Keeps path + essential query (YouTube needs `?v=…`); only strips tracking params.
 */
export function normalizeMediaUrl(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed) return null;

  const cleanUrl = (url: string) => {
    try {
      const parsed = new URL(url);
      // Do NOT wipe the whole query: youtube.com/watch without ?v= becomes
      // feed/recommended and probes as title "recommended".
      for (const key of [...parsed.searchParams.keys()]) {
        const lower = key.toLowerCase();
        if (DROP_QUERY_PARAMS.has(lower) || lower.startsWith('utm_')) {
          parsed.searchParams.delete(key);
        }
      }
      return parsed.toString();
    } catch {
      return url;
    }
  };

  if (/^https?:\/\//i.test(trimmed)) {
    return cleanUrl(trimmed);
  }

  // spotify:track:xxx → https://open.spotify.com/track/xxx
  const spotifyUri = trimmed.match(
    /^spotify:(track|album|playlist|artist|episode|show):([a-zA-Z0-9]+)/i
  );
  if (spotifyUri) {
    return `https://open.spotify.com/${spotifyUri[1].toLowerCase()}/${spotifyUri[2]}`;
  }

  if (/^www\./i.test(trimmed)) {
    return cleanUrl(`https://${trimmed}`);
  }

  return null;
}

/** YouTube / YouTube Music / youtu.be — needs a JS runtime and often a signed-in session. */
export function isYoutubeMediaUrl(text: string): boolean {
  const url = normalizeMediaUrl(text) ?? text.trim();
  try {
    const host = new URL(url).hostname.toLowerCase().replace(/^www\./, '');
    return host === 'youtu.be' || host === 'youtube.com' || host.endsWith('.youtube.com');
  } catch {
    const lower = url.toLowerCase();
    return lower.includes('youtube.com') || lower.includes('youtu.be');
  }
}

function isYoutubeAuthErrorMessage(message: string): boolean {
  const lower = message.toLowerCase();
  return (
    lower.includes('bot check')
    || lower.includes('not a bot')
    || lower.includes('sign in to youtube')
    || lower.includes('youtube sign-in')
  );
}

export function needsYoutubeSignIn(message: string, url: string): boolean {
  return isYoutubeMediaUrl(url) && isYoutubeAuthErrorMessage(message);
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