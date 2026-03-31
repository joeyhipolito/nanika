package api

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"google.golang.org/api/calendar/v3"
	"google.golang.org/api/option"
)

// newTestCalendarClient creates a Client backed by a test HTTP server.
// The provided handler serves all calendar API requests.
func newTestCalendarClient(t *testing.T, handler http.Handler) *Client {
	t.Helper()
	srv := httptest.NewServer(handler)
	t.Cleanup(srv.Close)

	calsvc, err := calendar.NewService(context.Background(),
		option.WithoutAuthentication(),
		option.WithEndpoint(srv.URL+"/"),
	)
	if err != nil {
		t.Fatalf("create test calendar service: %v", err)
	}
	return &Client{calsvc: calsvc, alias: "test"}
}

// eventsResponse builds a minimal calendar#events JSON response.
func eventsResponse(items []map[string]interface{}) map[string]interface{} {
	return map[string]interface{}{
		"kind":  "calendar#events",
		"items": items,
	}
}

func TestListUpcomingEvents_empty(t *testing.T) {
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(eventsResponse(nil))
	})
	c := newTestCalendarClient(t, handler)

	events, err := c.ListUpcomingEvents(10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(events) != 0 {
		t.Errorf("got %d events; want 0", len(events))
	}
}

func TestListUpcomingEvents_timedAndAllDay(t *testing.T) {
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(eventsResponse([]map[string]interface{}{
			{
				"id":      "evt1",
				"summary": "Team standup",
				"start":   map[string]string{"dateTime": "2026-03-19T09:00:00Z"},
				"end":     map[string]string{"dateTime": "2026-03-19T09:30:00Z"},
			},
			{
				"id":      "evt2",
				"summary": "All-day holiday",
				"start":   map[string]string{"date": "2026-03-20"},
				"end":     map[string]string{"date": "2026-03-21"},
			},
		}))
	})
	c := newTestCalendarClient(t, handler)

	events, err := c.ListUpcomingEvents(10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(events) != 2 {
		t.Fatalf("got %d events; want 2", len(events))
	}

	timed := events[0]
	if timed.ID != "evt1" {
		t.Errorf("events[0].ID = %q; want %q", timed.ID, "evt1")
	}
	if timed.Summary != "Team standup" {
		t.Errorf("events[0].Summary = %q; want %q", timed.Summary, "Team standup")
	}
	if timed.Start != "2026-03-19T09:00:00Z" {
		t.Errorf("events[0].Start = %q; want %q", timed.Start, "2026-03-19T09:00:00Z")
	}
	if timed.AllDay {
		t.Error("events[0].AllDay = true; want false")
	}
	if timed.Account != "test" {
		t.Errorf("events[0].Account = %q; want %q", timed.Account, "test")
	}

	allDay := events[1]
	if !allDay.AllDay {
		t.Error("events[1].AllDay = false; want true")
	}
	if allDay.Start != "2026-03-20" {
		t.Errorf("events[1].Start = %q; want %q", allDay.Start, "2026-03-20")
	}
}

func TestListUpcomingEvents_defaultLimit(t *testing.T) {
	var gotMaxResults string
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotMaxResults = r.URL.Query().Get("maxResults")
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(eventsResponse(nil))
	})
	c := newTestCalendarClient(t, handler)

	// limit=0 should default to 10
	if _, err := c.ListUpcomingEvents(0); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if gotMaxResults != "10" {
		t.Errorf("maxResults = %q; want %q", gotMaxResults, "10")
	}
}

func TestCreateEvent_success(t *testing.T) {
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			http.Error(w, "expected POST", http.StatusMethodNotAllowed)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]interface{}{
			"id":       "new-evt-123",
			"summary":  "Planning meeting",
			"start":    map[string]string{"dateTime": "2026-03-20T14:00:00Z"},
			"end":      map[string]string{"dateTime": "2026-03-20T15:00:00Z"},
			"htmlLink": "https://calendar.google.com/event?eid=xxx",
		})
	})
	c := newTestCalendarClient(t, handler)

	ev, err := c.CreateEvent(CreateEventParams{
		Summary: "Planning meeting",
		Start:   "2026-03-20T14:00:00Z",
		End:     "2026-03-20T15:00:00Z",
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if ev.ID != "new-evt-123" {
		t.Errorf("ev.ID = %q; want %q", ev.ID, "new-evt-123")
	}
	if ev.Summary != "Planning meeting" {
		t.Errorf("ev.Summary = %q; want %q", ev.Summary, "Planning meeting")
	}
	if ev.HtmlLink == "" {
		t.Error("ev.HtmlLink is empty; want non-empty")
	}
	if ev.Account != "test" {
		t.Errorf("ev.Account = %q; want %q", ev.Account, "test")
	}
}

func TestCreateEvent_validationErrors(t *testing.T) {
	tests := []struct {
		name   string
		params CreateEventParams
	}{
		{"missing summary", CreateEventParams{Start: "2026-03-20T14:00:00Z", End: "2026-03-20T15:00:00Z"}},
		{"missing start", CreateEventParams{Summary: "Test", End: "2026-03-20T15:00:00Z"}},
		{"missing end", CreateEventParams{Summary: "Test", Start: "2026-03-20T14:00:00Z"}},
	}
	// No API calls expected — validation fires before the network.
	c := &Client{alias: "test"}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := c.CreateEvent(tt.params)
			if err == nil {
				t.Error("expected error; got nil")
			}
		})
	}
}

func TestCheckAvailability_available(t *testing.T) {
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]interface{}{
			"kind": "calendar#freeBusy",
			"calendars": map[string]interface{}{
				"primary": map[string]interface{}{
					"busy": []interface{}{},
				},
			},
		})
	})
	c := newTestCalendarClient(t, handler)

	start := time.Date(2026, 3, 19, 9, 0, 0, 0, time.UTC)
	end := time.Date(2026, 3, 19, 10, 0, 0, 0, time.UTC)
	fb, err := c.CheckAvailability(start, end)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !fb.Available {
		t.Error("Available = false; want true")
	}
	if len(fb.BusySlots) != 0 {
		t.Errorf("BusySlots = %d; want 0", len(fb.BusySlots))
	}
	if fb.Account != "test" {
		t.Errorf("Account = %q; want %q", fb.Account, "test")
	}
}

func TestCheckAvailability_busy(t *testing.T) {
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]interface{}{
			"kind": "calendar#freeBusy",
			"calendars": map[string]interface{}{
				"primary": map[string]interface{}{
					"busy": []interface{}{
						map[string]string{
							"start": "2026-03-19T09:00:00Z",
							"end":   "2026-03-19T09:30:00Z",
						},
					},
				},
			},
		})
	})
	c := newTestCalendarClient(t, handler)

	start := time.Date(2026, 3, 19, 9, 0, 0, 0, time.UTC)
	end := time.Date(2026, 3, 19, 10, 0, 0, 0, time.UTC)
	fb, err := c.CheckAvailability(start, end)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if fb.Available {
		t.Error("Available = true; want false")
	}
	if len(fb.BusySlots) != 1 {
		t.Fatalf("BusySlots = %d; want 1", len(fb.BusySlots))
	}
	if fb.BusySlots[0].Start != "2026-03-19T09:00:00Z" {
		t.Errorf("BusySlots[0].Start = %q; want %q", fb.BusySlots[0].Start, "2026-03-19T09:00:00Z")
	}
	if fb.BusySlots[0].End != "2026-03-19T09:30:00Z" {
		t.Errorf("BusySlots[0].End = %q; want %q", fb.BusySlots[0].End, "2026-03-19T09:30:00Z")
	}
}

func TestCheckAvailability_endBeforeStart(t *testing.T) {
	c := &Client{alias: "test"}
	start := time.Date(2026, 3, 19, 10, 0, 0, 0, time.UTC)
	end := time.Date(2026, 3, 19, 9, 0, 0, 0, time.UTC) // end before start
	_, err := c.CheckAvailability(start, end)
	if err == nil {
		t.Error("expected error for end before start; got nil")
	}
}

func TestToCalendarEvent_allDay(t *testing.T) {
	item := &calendar.Event{
		Id:      "evt-allday",
		Summary: "Company holiday",
		Start:   &calendar.EventDateTime{Date: "2026-03-20"},
		End:     &calendar.EventDateTime{Date: "2026-03-21"},
	}
	ev := toCalendarEvent("work", item)
	if !ev.AllDay {
		t.Error("AllDay = false; want true")
	}
	if ev.Start != "2026-03-20" {
		t.Errorf("Start = %q; want %q", ev.Start, "2026-03-20")
	}
	if ev.End != "2026-03-21" {
		t.Errorf("End = %q; want %q", ev.End, "2026-03-21")
	}
	if ev.Account != "work" {
		t.Errorf("Account = %q; want %q", ev.Account, "work")
	}
}

func TestToCalendarEvent_withAttendees(t *testing.T) {
	item := &calendar.Event{
		Id:      "evt-meeting",
		Summary: "Planning",
		Start:   &calendar.EventDateTime{DateTime: "2026-03-19T14:00:00Z"},
		End:     &calendar.EventDateTime{DateTime: "2026-03-19T15:00:00Z"},
		Attendees: []*calendar.EventAttendee{
			{Email: "alice@example.com"},
			{Email: "bob@example.com"},
		},
	}
	ev := toCalendarEvent("personal", item)
	if len(ev.Attendees) != 2 {
		t.Fatalf("len(Attendees) = %d; want 2", len(ev.Attendees))
	}
	if ev.Attendees[0] != "alice@example.com" {
		t.Errorf("Attendees[0] = %q; want %q", ev.Attendees[0], "alice@example.com")
	}
	if ev.Attendees[1] != "bob@example.com" {
		t.Errorf("Attendees[1] = %q; want %q", ev.Attendees[1], "bob@example.com")
	}
	if ev.AllDay {
		t.Error("AllDay = true; want false for timed event")
	}
}

func TestToCalendarEvent_nilStartEnd(t *testing.T) {
	item := &calendar.Event{
		Id:      "evt-nostartend",
		Summary: "Weird event",
	}
	ev := toCalendarEvent("test", item)
	if ev.Start != "" {
		t.Errorf("Start = %q; want empty for nil Start", ev.Start)
	}
	if ev.End != "" {
		t.Errorf("End = %q; want empty for nil End", ev.End)
	}
}
