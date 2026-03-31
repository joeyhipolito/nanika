package api

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
)

// GetVoices calls GET /v1/voices and returns all available voices.
func (c *Client) GetVoices(ctx context.Context) ([]Voice, error) {
	req, err := c.newRequest(ctx, http.MethodGet, "/v1/voices")
	if err != nil {
		return nil, err
	}

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("GET /v1/voices: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("GET /v1/voices returned status %d", resp.StatusCode)
	}

	var vr VoicesResponse
	if err := json.NewDecoder(resp.Body).Decode(&vr); err != nil {
		return nil, fmt.Errorf("decoding voices response: %w", err)
	}
	return vr.Voices, nil
}

// GetUser calls GET /v1/user to verify the API key and fetch quota info.
func (c *Client) GetUser(ctx context.Context) (UserResponse, error) {
	req, err := c.newRequest(ctx, http.MethodGet, "/v1/user")
	if err != nil {
		return UserResponse{}, err
	}

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return UserResponse{}, fmt.Errorf("GET /v1/user: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusUnauthorized {
		return UserResponse{}, fmt.Errorf("invalid API key (401 Unauthorized)")
	}
	if resp.StatusCode != http.StatusOK {
		return UserResponse{}, fmt.Errorf("GET /v1/user returned status %d", resp.StatusCode)
	}

	var ur UserResponse
	if err := json.NewDecoder(resp.Body).Decode(&ur); err != nil {
		return UserResponse{}, fmt.Errorf("decoding user response: %w", err)
	}
	return ur, nil
}
