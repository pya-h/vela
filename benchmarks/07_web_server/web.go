package main

import (
	"log"
	"net/http"
)

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/plain")
		w.Header().Set("Connection", "close")
		_, _ = w.Write([]byte("ok"))
	})
	mux.HandleFunc("/json", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Connection", "close")
		_, _ = w.Write([]byte(`{"status":"ok","value":42}`))
	})

	srv := &http.Server{
		Addr:    "127.0.0.1:18260",
		Handler: mux,
	}
	log.Fatal(srv.ListenAndServe())
}
