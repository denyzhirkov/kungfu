# Environment overrides
region      = "eu-west-1"
env         = "prod"
db_password = "hunter2-prod-secret"
subnets = [
  "10.0.1.0/24",
  "10.0.2.0/24",
  "10.0.3.0/24",
]
tags = {
  Team = "platform"
  Cost = "42"
}
