-- Supabase Schema Initialization for Luma
-- Use this script in the Supabase SQL Editor

-- 1. Enable UUID extension
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- 2. Venues Table
CREATE TABLE venues (
    id UUID PRIMARY KEY, -- Maps to remote_id in local DB
    uid UUID NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Enable RLS for venues
ALTER TABLE venues ENABLE ROW LEVEL SECURITY;

CREATE POLICY "Users can manage their own venues" 
    ON venues FOR ALL 
    USING (auth.uid() = uid);

-- 3. Patterns Table
CREATE TABLE patterns (
    id UUID PRIMARY KEY, -- Maps to remote_id in local DB
    uid UUID NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    category_id BIGINT, -- Simplified for MVP
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Enable RLS for patterns
ALTER TABLE patterns ENABLE ROW LEVEL SECURITY;

CREATE POLICY "Users can manage their own patterns" 
    ON patterns FOR ALL 
    USING (auth.uid() = uid);

-- 4. Tracks Table
CREATE TABLE tracks (
    id UUID PRIMARY KEY, -- Maps to remote_id in local DB
    uid UUID NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
    track_hash TEXT NOT NULL,
    title TEXT,
    artist TEXT,
    album TEXT,
    track_number BIGINT,
    disc_number BIGINT,
    duration_seconds DOUBLE PRECISION,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Enable RLS for tracks
ALTER TABLE tracks ENABLE ROW LEVEL SECURITY;

CREATE POLICY "Users can manage their own tracks" 
    ON tracks FOR ALL 
    USING (auth.uid() = uid);

-- 5. Auto-update updated_at function
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ language 'plpgsql';

CREATE TRIGGER update_venues_updated_at BEFORE UPDATE ON venues FOR EACH ROW EXECUTE PROCEDURE update_updated_at_column();
CREATE TRIGGER update_patterns_updated_at BEFORE UPDATE ON patterns FOR EACH ROW EXECUTE PROCEDURE update_updated_at_column();
CREATE TRIGGER update_tracks_updated_at BEFORE UPDATE ON tracks FOR EACH ROW EXECUTE PROCEDURE update_updated_at_column();
