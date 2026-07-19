# Rina Halim — Backend Engineer

A CV-shaped document, because that is what this system was actually tested against during
development, and because a CV is the worst case for a fixed-width chunker: dense, list-heavy, and
full of facts that lose their subject the moment they are separated from the heading above them.

## Contact

Email rina.halim@example.test. Based in Bandung, Indonesia. Available for remote work across
European time zones.

## Summary

Backend engineer with seven years of experience, mostly in Go and Rust. Comfortable owning a service
end to end: schema design, the HTTP surface, the queue topology behind it, and the on-call rota.

## Experience

**Senior Backend Engineer, Trellis Logistics (2022 to present).** Rebuilt the shipment tracking
pipeline from a nightly batch into an event-driven system on RabbitMQ, cutting the delay between a
carrier scan and a customer-visible update from 14 hours to under 3 minutes.

**Backend Engineer, Kirana Payments (2019 to 2022).** Built the reconciliation service that matches
settlement files against internal ledger entries. Reduced unmatched transactions from 2.1 percent to
0.04 percent by handling partial settlements the previous implementation discarded.

**Junior Developer, Sinar Data (2017 to 2019).** Maintained a PHP reporting application and migrated
its scheduled jobs off cron onto a queue.

## Education

Bachelor of Informatics Engineering, Institut Teknologi Bandung, 2017.

## Technical skills

Go, Rust, TypeScript. PostgreSQL and its row-level security model. RabbitMQ, Kafka. Qdrant and
pgvector for retrieval work. Docker, and enough Kubernetes to be dangerous.
