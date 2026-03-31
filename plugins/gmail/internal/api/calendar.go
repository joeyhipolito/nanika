package api

import (
	"fmt"
	"time"

	"google.golang.org/api/calendar/v3"
)

// ListUpcomingEvents returns up to limit events from the primary calendar
// starting from now, ordered by start time.
func (c *Client) ListUpcomingEvents(limit int) ([]CalendarEvent, error) {
	if limit <= 0 {
		limit = 10
	}
	now := time.Now().Format(time.RFC3339)

	var events []CalendarEvent
	err := withRetry(func() error {
		resp, err := c.calsvc.Events.List("primary").
			TimeMin(now).
			MaxResults(int64(limit)).
			SingleEvents(true).
			OrderBy("startTime").
			Do()
		if err != nil {
			return err
		}
		events = make([]CalendarEvent, 0, len(resp.Items))
		for _, item := range resp.Items {
			events = append(events, toCalendarEvent(c.alias, item))
		}
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("listing events for %s: %w", c.alias, err)
	}
	return events, nil
}

// CreateEvent creates a new event on the primary calendar.
func (c *Client) CreateEvent(params CreateEventParams) (*CalendarEvent, error) {
	if params.Summary == "" {
		return nil, fmt.Errorf("summary is required")
	}
	if params.Start == "" {
		return nil, fmt.Errorf("start time is required")
	}
	if params.End == "" {
		return nil, fmt.Errorf("end time is required")
	}

	ev := &calendar.Event{
		Summary:     params.Summary,
		Description: params.Description,
		Location:    params.Location,
		Start: &calendar.EventDateTime{
			DateTime: params.Start,
			TimeZone: params.TimeZone,
		},
		End: &calendar.EventDateTime{
			DateTime: params.End,
			TimeZone: params.TimeZone,
		},
	}
	for _, a := range params.Attendees {
		ev.Attendees = append(ev.Attendees, &calendar.EventAttendee{Email: a})
	}

	var created CalendarEvent
	err := withRetry(func() error {
		result, err := c.calsvc.Events.Insert("primary", ev).Do()
		if err != nil {
			return err
		}
		created = toCalendarEvent(c.alias, result)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("creating event for %s: %w", c.alias, err)
	}
	return &created, nil
}

// CheckAvailability returns free/busy information for the primary calendar
// within the given time range.
func (c *Client) CheckAvailability(start, end time.Time) (*FreeBusy, error) {
	if !end.After(start) {
		return nil, fmt.Errorf("end time must be after start time")
	}

	req := &calendar.FreeBusyRequest{
		TimeMin: start.Format(time.RFC3339),
		TimeMax: end.Format(time.RFC3339),
		Items:   []*calendar.FreeBusyRequestItem{{Id: "primary"}},
	}

	fb := &FreeBusy{
		Account: c.alias,
		TimeMin: req.TimeMin,
		TimeMax: req.TimeMax,
	}

	err := withRetry(func() error {
		resp, err := c.calsvc.Freebusy.Query(req).Do()
		if err != nil {
			return err
		}
		if cal, ok := resp.Calendars["primary"]; ok {
			fb.BusySlots = make([]FreeBusySlot, len(cal.Busy))
			for i, b := range cal.Busy {
				fb.BusySlots[i] = FreeBusySlot{Start: b.Start, End: b.End}
			}
		}
		fb.Available = len(fb.BusySlots) == 0
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("checking availability for %s: %w", c.alias, err)
	}
	return fb, nil
}

// toCalendarEvent converts a *calendar.Event to our CalendarEvent type.
// All-day events use Date (YYYY-MM-DD); timed events use DateTime (RFC3339).
func toCalendarEvent(alias string, item *calendar.Event) CalendarEvent {
	ev := CalendarEvent{
		ID:          item.Id,
		Account:     alias,
		Summary:     item.Summary,
		Description: item.Description,
		Location:    item.Location,
		HtmlLink:    item.HtmlLink,
		Status:      item.Status,
	}
	if item.Start != nil {
		if item.Start.DateTime != "" {
			ev.Start = item.Start.DateTime
		} else {
			ev.Start = item.Start.Date
			ev.AllDay = true
		}
	}
	if item.End != nil {
		if item.End.DateTime != "" {
			ev.End = item.End.DateTime
		} else {
			ev.End = item.End.Date
		}
	}
	for _, a := range item.Attendees {
		ev.Attendees = append(ev.Attendees, a.Email)
	}
	return ev
}
