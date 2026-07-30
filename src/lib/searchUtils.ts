import type { MusicFile, Playlist } from '$lib/stores/player.svelte';
import { trackDisplayArtist, trackDisplayTitle } from '$lib/stores/player.svelte';
import { looksLikeMediaUrl } from '$lib/urlUtils';

export type SearchField = 'both' | 'title' | 'artist';

export interface ParsedSearchQuery {
  text: string;
  field: SearchField;
  playlistFilter: string | null;
}

export interface SearchResultItem {
  track: MusicFile;
  playlistId: string;
  playlistName: string;
}

export type SuggestionKind = 'modifier' | 'playlist';

export interface SearchSuggestion {
  kind: SuggestionKind;
  label: string;
  detail?: string;
  insert: string;
  replaceFrom: number;
  replaceTo: number;
}

const MODIFIER_REGEX = /@p=([^@]+)|@p\s+([^@]+)|@artist\b|@a\b|@title\b|@t\b/gi;

export function isTrackSearch(query: string): boolean {
  return query.trim().length > 0 && !looksLikeMediaUrl(query);
}

function splitPlaylistFilterAndText(
  value: string,
  playlists: Playlist[],
): { filter: string; text: string } {
  const trimmed = value.trim();
  if (!trimmed) return { filter: '', text: '' };

  const words = trimmed.split(/\s+/);
  let bestFilter = '';
  let bestText = trimmed;

  for (let i = 1; i <= words.length; i++) {
    const candidate = words.slice(0, i).join(' ');
    const q = candidate.toLowerCase();
    const matches = playlists.filter((p) => p.name.toLowerCase().includes(q));
    if (matches.length > 0) {
      bestFilter = candidate;
      bestText = words.slice(i).join(' ');
    }
  }

  return { filter: bestFilter, text: bestText };
}

export function parseSearchQuery(raw: string, playlists: Playlist[] = []): ParsedSearchQuery {
  let field: SearchField = 'both';
  let playlistFilter: string | null = null;
  const textParts: string[] = [];

  let lastIndex = 0;
  let match: RegExpExecArray | null;
  const regex = new RegExp(MODIFIER_REGEX.source, 'gi');

  while ((match = regex.exec(raw)) !== null) {
    const before = raw.slice(lastIndex, match.index).trim();
    if (before) textParts.push(before);

    const token = match[0].toLowerCase();
    if (token.startsWith('@p')) {
      const rawValue = (match[1] ?? match[2] ?? '').trim();
      const split = splitPlaylistFilterAndText(rawValue, playlists);
      playlistFilter = split.filter || rawValue;
      if (split.text) textParts.push(split.text);
    } else if (token === '@artist' || token === '@a') {
      field = 'artist';
    } else if (token === '@title' || token === '@t') {
      field = 'title';
    }

    lastIndex = match.index + match[0].length;
  }

  const tail = raw.slice(lastIndex).trim();
  if (tail) textParts.push(tail);

  return {
    text: textParts.join(' ').trim(),
    field,
    playlistFilter,
  };
}

function matchingPlaylists(playlists: Playlist[], filter: string): Playlist[] {
  const q = filter.toLowerCase().trim();
  if (!q) return playlists;
  return playlists.filter((p) => p.name.toLowerCase().includes(q));
}

function trackMatches(track: MusicFile, q: string, field: SearchField): boolean {
  const title = trackDisplayTitle(track).toLowerCase();
  const artist = trackDisplayArtist(track).toLowerCase();
  if (field === 'title') return title.includes(q);
  if (field === 'artist') return artist.includes(q);
  return title.includes(q) || artist.includes(q);
}

export function searchTracks(playlists: Playlist[], query: string): SearchResultItem[] {
  const parsed = parseSearchQuery(query, playlists);
  if (!parsed.text) return [];

  const targetPlaylists = parsed.playlistFilter
    ? matchingPlaylists(playlists, parsed.playlistFilter)
    : playlists;

  if (parsed.playlistFilter && targetPlaylists.length === 0) return [];

  const q = parsed.text.toLowerCase();
  const results: SearchResultItem[] = [];

  for (const playlist of targetPlaylists) {
    for (const track of playlist.tracks) {
      if (trackMatches(track, q, parsed.field)) {
        results.push({
          track,
          playlistId: playlist.id,
          playlistName: playlist.name,
        });
      }
    }
  }

  return results;
}

function modifierSuggestions(partial: string, replaceFrom: number, cursorPos: number): SearchSuggestion[] {
  const p = partial.toLowerCase();
  const options: SearchSuggestion[] = [];

  const candidates: Array<{ label: string; detail: string; insert: string; prefixes: string[] }> = [
    { label: '@artist', detail: 'Search by artist', insert: '@artist ', prefixes: ['artist', 'art', 'ar'] },
    { label: '@a', detail: 'Search by artist', insert: '@a ', prefixes: ['a'] },
    { label: '@title', detail: 'Search by title', insert: '@title ', prefixes: ['title', 'tit', 'ti'] },
    { label: '@t', detail: 'Search by title', insert: '@t ', prefixes: ['t'] },
    { label: '@p=', detail: 'Search in playlist', insert: '@p=', prefixes: ['p', 'pl', 'play', 'playl'] },
  ];

  for (const c of candidates) {
    if (!p || c.prefixes.some((prefix) => prefix.startsWith(p)) || c.label.slice(1).startsWith(p)) {
      options.push({
        kind: 'modifier',
        label: c.label,
        detail: c.detail,
        insert: c.insert,
        replaceFrom,
        replaceTo: cursorPos,
      });
    }
  }

  return options;
}

function playlistSuggestions(
  partial: string,
  playlists: Playlist[],
  replaceFrom: number,
  cursorPos: number,
): SearchSuggestion[] {
  const q = partial.toLowerCase();
  return playlists
    .filter((pl) => !q || pl.name.toLowerCase().includes(q))
    .slice(0, 8)
    .map((pl) => ({
      kind: 'playlist' as const,
      label: pl.name,
      detail: 'Playlist',
      insert: `${pl.name} `,
      replaceFrom,
      replaceTo: cursorPos,
    }));
}

export function getSearchSuggestions(
  query: string,
  cursorPos: number,
  playlists: Playlist[],
): SearchSuggestion[] {
  const before = query.slice(0, cursorPos);

  const modMatch = before.match(/@([a-z]*)$/i);
  if (modMatch) {
    const replaceFrom = before.length - modMatch[0].length;
    return modifierSuggestions(modMatch[1], replaceFrom, cursorPos);
  }

  const pEqMatch = before.match(/@p=([^@]*)$/i);
  if (pEqMatch) {
    const replaceFrom = before.length - pEqMatch[1].length;
    return playlistSuggestions(pEqMatch[1], playlists, replaceFrom, cursorPos);
  }

  const pSpaceMatch = before.match(/@p\s+([^@]*)$/i);
  if (pSpaceMatch) {
    const replaceFrom = before.length - pSpaceMatch[1].length;
    return playlistSuggestions(pSpaceMatch[1], playlists, replaceFrom, cursorPos);
  }

  return [];
}

export function applySuggestion(query: string, suggestion: SearchSuggestion): string {
  return query.slice(0, suggestion.replaceFrom) + suggestion.insert + query.slice(suggestion.replaceTo);
}

export function describeSearchQuery(query: string, playlists: Playlist[] = []): string {
  const parsed = parseSearchQuery(query, playlists);
  const parts: string[] = [];

  if (parsed.field === 'artist') parts.push('artist');
  else if (parsed.field === 'title') parts.push('title');
  else parts.push('title or artist');

  if (parsed.playlistFilter) {
    parts.push(`in "${parsed.playlistFilter}"`);
  } else {
    parts.push('all playlists');
  }

  return parts.join(' · ');
}